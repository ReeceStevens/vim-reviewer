extern crate git2;
extern crate nvim_oxi;
extern crate regex;
extern crate reqwest;
extern crate serde;
extern crate tempfile;
extern crate toml;

use git2::Repository;
use nvim_oxi::api;
use nvim_oxi::api::opts::{CmdOpts, CreateCommandOpts};
use nvim_oxi::api::types::{CmdInfos, CommandArgs, CommandNArgs, CommandRange};
use nvim_oxi::string;
use nvim_oxi::{self as oxi, Array, Dictionary, Object};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use std::env;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

macro_rules! create_command {
    ($name:expr, $desc:expr, $nargs:expr, $fn:expr) => {
        let opts = CreateCommandOpts::builder()
            .desc($desc)
            .nargs($nargs)
            .range(CommandRange::CurrentLine)
            .build();
        api::create_user_command($name, $fn, &opts)?;
    };
}

type ApiResult<T> = std::result::Result<T, api::Error>;

/// Git backend type (GitHub or GitLab)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum GitBackend {
    GitHub,
    GitLab,
}

/// Configuration structure for vim-reviewer.toml file
#[derive(Deserialize, Debug)]
struct TomlConfig {
    backend: TomlBackendConfig,
}

#[derive(Deserialize, Debug)]
struct TomlBackendConfig {
    #[serde(rename = "type")]
    backend_type: String,
    url: Option<String>,
    token: String,
}

/// Based on the remote URL, parse out the repository name, owner, and backend type.
///
/// Supports both SSH and HTTPS URLs for GitHub and GitLab.
/// Examples:
/// - git@github.com:owner/repo.git -> (owner, repo, GitHub)
/// - https://github.com/owner/repo.git -> (owner, repo, GitHub)
/// - git@gitlab.com:owner/repo.git -> (owner, repo, GitLab)
/// - https://gitlab.com/owner/repo.git -> (owner, repo, GitLab)
fn parse_config_from_url(url: &str) -> Result<(String, String, GitBackend), String> {
    // Determine backend from URL
    let backend = if url.contains("gitlab") {
        GitBackend::GitLab
    } else if url.contains("github") {
        GitBackend::GitHub
    } else {
        return Err("Could not determine git backend (GitHub or GitLab) from URL".to_string());
    };

    // Parse SSH format (git@host:owner/repo.git)
    if url.contains("@") && url.contains(":") && !url.contains("://") {
        let repository_info = url.split(":").last();
        let results = match repository_info {
            Some(info) => info.split("/").collect::<Vec<&str>>(),
            None => return Err("Invalid repository url".to_string()),
        };
        if results.len() < 2 {
            return Err("Invalid repository url format".to_string());
        }
        return Ok((
            results[0].to_string(),
            results[1].to_string().replace(".git", ""),
            backend,
        ));
    }

    // Parse HTTPS format (https://host/owner/repo.git)
    if url.contains("://") {
        let parts: Vec<&str> = url.split("://").collect();
        if parts.len() < 2 {
            return Err("Invalid HTTPS repository url".to_string());
        }
        let path_parts: Vec<&str> = parts[1].split("/").collect();
        if path_parts.len() < 3 {
            return Err("Invalid HTTPS repository url format".to_string());
        }
        return Ok((
            path_parts[1].to_string(),
            path_parts[2].to_string().replace(".git", ""),
            backend,
        ));
    }

    Err("Unsupported repository URL format".to_string())
}

/// Load configuration from vim-reviewer.toml in the current working directory, if it exists.
/// Returns Some((owner, repo, backend, backend_url, token)) if the file exists and is valid, None otherwise.
fn load_toml_config() -> Option<(String, String, GitBackend, Option<String>, String)> {
    let config_path = env::current_dir().ok()?.join("vim-reviewer.toml");

    if !config_path.exists() {
        return None;
    }

    let mut config_contents = String::new();
    let mut file = match File::open(&config_path) {
        Ok(f) => f,
        Err(e) => {
            api::err_writeln(&format!("Failed to open vim-reviewer.toml: {}", e));
            return None;
        }
    };

    if let Err(e) = file.read_to_string(&mut config_contents) {
        api::err_writeln(&format!("Failed to read vim-reviewer.toml: {}", e));
        return None;
    }

    let toml_config: TomlConfig = match toml::from_str(&config_contents) {
        Ok(config) => config,
        Err(e) => {
            api::err_writeln(&format!("Failed to parse vim-reviewer.toml: {}", e));
            return None;
        }
    };

    // Determine backend type
    let backend = match toml_config.backend.backend_type.to_lowercase().as_str() {
        "github" => GitBackend::GitHub,
        "gitlab" => GitBackend::GitLab,
        _ => {
            api::err_writeln(&format!(
                "Invalid backend type '{}' in vim-reviewer.toml. Must be 'github' or 'gitlab'.",
                toml_config.backend.backend_type
            ));
            return None;
        }
    };

    // Extract owner, repo, and base URL from the config
    let (owner, repo, backend_url) = if let Some(url) = toml_config.backend.url {
        match parse_config_from_url(&url) {
            Ok((o, r, _)) => {
                // Extract base URL (scheme + host) from the full URL
                let base_url = if url.contains("://") {
                    let parts: Vec<&str> = url.split("://").collect();
                    if parts.len() >= 2 {
                        let host_parts: Vec<&str> = parts[1].split("/").collect();
                        Some(format!("{}://{}", parts[0], host_parts[0]))
                    } else {
                        None
                    }
                } else {
                    None
                };
                (o, r, base_url)
            }
            Err(e) => {
                api::err_writeln(&format!(
                    "Failed to parse URL from vim-reviewer.toml: {}",
                    e
                ));
                return None;
            }
        }
    } else {
        // If no URL provided, fall back to detecting from git remote
        let current_dir = match env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                api::err_writeln(&format!("Failed to get current directory: {}", e));
                return None;
            }
        };
        let repo = match Repository::open(current_dir) {
            Ok(r) => r,
            Err(e) => {
                api::err_writeln(&format!(
                    "No URL in vim-reviewer.toml and current directory is not a git repository: {}",
                    e
                ));
                return None;
            }
        };
        let remote_url = match repo.find_remote("origin") {
            Ok(remote) => match remote.url() {
                Some(url) => url.to_string(),
                None => {
                    api::err_writeln("Remote 'origin' has no URL");
                    return None;
                }
            },
            Err(e) => {
                api::err_writeln(&format!(
                    "No URL in vim-reviewer.toml and failed to find remote 'origin': {}",
                    e
                ));
                return None;
            }
        };
        match parse_config_from_url(&remote_url) {
            Ok((o, r, _)) => (o, r, None),
            Err(e) => {
                api::err_writeln(&format!(
                    "Failed to parse repository information from remote URL: {}",
                    e
                ));
                return None;
            }
        }
    };

    Some((owner, repo, backend, backend_url, toml_config.backend.token))
}

/// Update the repository configuration based on vim-reviewer.toml if present,
/// otherwise fall back to detecting from the current origin remote
fn update_config_from_remote() -> oxi::Result<()> {
    // First, try to load config from vim-reviewer.toml
    if let Some((owner, repo_name, backend, backend_url, token)) = load_toml_config() {
        // Store the token from TOML config as an environment variable
        // This allows the rest of the code to use it transparently
        let token_var = match &backend {
            GitBackend::GitHub => "GH_REVIEW_API_TOKEN",
            GitBackend::GitLab => "GITLAB_TOKEN",
        };
        unsafe {
            env::set_var(token_var, token);
        }

        update_configuration(Config {
            owner,
            repo: repo_name,
            backend,
            backend_url,
            active_pr: None,
        });

        return Ok(());
    }

    // Fall back to detecting from git remote
    let current_dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            api::err_writeln(&format!("Failed to get current directory: {}", e));
            return Ok(());
        }
    };
    let repo = match Repository::open(&current_dir) {
        Ok(repo) => repo,
        Err(e) => {
            api::err_writeln(&format!("Current directory is not a git repository: {}", e));
            return Ok(());
        }
    };
    let remote_url = match repo.find_remote("origin") {
        Ok(remote) => match remote.url() {
            Some(url) => url.to_string(),
            None => {
                api::err_writeln("Remote 'origin' has no URL");
                return Ok(());
            }
        },
        Err(e) => {
            api::err_writeln(&format!("Failed to find remote 'origin': {}", e));
            return Ok(());
        }
    };
    let (owner, repo_name, backend) = match parse_config_from_url(&remote_url) {
        Ok(results) => results,
        Err(e) => {
            api::err_writeln(&format!(
                "Failed to parse repository information from remote URL: {}",
                e
            ));
            return Ok(());
        }
    };

    update_configuration(Config {
        owner,
        repo: repo_name,
        backend,
        backend_url: None,
        active_pr: None,
    });

    Ok(())
}

#[oxi::plugin]
fn vim_reviewer() -> oxi::Result<()> {
    update_config_from_remote()?;

    api::command("sign define PrReviewComment text=C> texthl=Search linehl=DiffText")?;

    create_command!(
        "UpdateReviewSigns",
        "Update the gutter symbols for review comments",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => return Ok(()),
                Some(review) => {
                    let mut sign_idx = 0;
                    api::command("sign unplace * group=PrReviewSigns")?;
                    let buffers = api::list_bufs();
                    for buffer in buffers {
                        unsafe {
                            let (_side, buffer_path) = get_current_buffer_path()?;

                            let obj: oxi::Object = (&buffer).into();
                            let handle = obj.as_integer_unchecked();

                            api::out_write(string!("{}\n", buffer_path.display()));
                            let comments_in_buffer: Vec<&Comment> = review
                                .comments
                                .iter()
                                .filter(|comment| {
                                    comment.path == buffer_path.to_str().unwrap().to_string()
                                })
                                .collect();
                            for comment in comments_in_buffer {
                                let start_line = comment.start_line.unwrap_or(comment.line);
                                let end_line = comment.line;
                                api::out_write(string!(
                                    "{:?}: {}-{}\n",
                                    buffer,
                                    start_line,
                                    end_line
                                ));
                                for line in start_line..=end_line {
                                    sign_idx += 1;
                                    let command = format!(
                                    "sign place {} line={} name=PrReviewComment group=PrReviewSigns buffer={}",
                                    sign_idx,
                                    line,
                                    handle,
                                );
                                    api::command(&command)?;
                                }
                            }
                        }
                    }
                    Ok(())
                }
            }
        }
    );

    create_command!(
        "StartReview",
        "Start a review",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            match get_config_from_file() {
                None => {
                    api::err_writeln("Could not read configuration file.");
                    return Ok(());
                }
                Some(mut config) => {
                    config.active_pr = Some(str::parse::<u32>(&args.args.unwrap()).unwrap());
                    update_configuration(config);
                    Ok(())
                }
            }
        }
    );

    create_command!(
        "PublishReview",
        "Publish a review to GitHub or GitLab",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                Some(review) => {
                    // Determine which token to use based on the backend
                    let (token_var, backend_name) = match review.backend {
                        GitBackend::GitHub => ("GH_REVIEW_API_TOKEN", "GitHub"),
                        GitBackend::GitLab => ("GITLAB_TOKEN", "GitLab"),
                    };

                    let token = match env::var(token_var) {
                        Ok(token) => token,
                        Err(e) => {
                            api::err_writeln(&format!(
                                "{} environment variable not set: {}",
                                token_var, e
                            ));
                            return Ok(());
                        }
                    };

                    match review.publish(token) {
                        Ok(response) => {
                            let status = response.status();
                            if status.is_success() {
                                api::out_write(string!(
                                    "Review published successfully to {}\n",
                                    backend_name
                                ));
                            } else {
                                api::err_writeln(&format!(
                                    "Failed to publish review to {} ({:?}): {:?}",
                                    backend_name,
                                    status,
                                    response.text()
                                ));
                            }
                        }
                        Err(error) => {
                            api::err_writeln(&format!(
                                "Failed to publish review to {} due to error: {}",
                                backend_name, error
                            ));
                        }
                    };
                    // TODO: Cleanup of current review
                    // update_signs();
                }
                None => {
                    api::err_writeln("Cannot publish since no review is currently active.");
                }
            };
            Ok(())
        }
    );

    create_command!(
        "ReviewComment",
        "Add a review comment",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No in-progress review");
                }
                Some(mut review) => {
                    if review.in_progress_comment.is_some() {
                        api::err_writeln("A review comment is already being edited.");
                        return Ok(());
                    }
                    let (side, path) = get_current_buffer_path()?;
                    let multi_line = args.line1 != args.line2;
                    review.in_progress_comment = Some(Comment::new(
                        "".to_string(),
                        args.line2 as u32,
                        path.to_str().unwrap().to_string(),
                        side,
                        Some(if multi_line {
                            args.line1 as u32
                        } else {
                            (args.line1 - 1) as u32
                        }),
                        Some(side),
                    ));
                    review.save();
                    new_temporary_buffer(Some("SaveComment new"))?;
                }
            }

            Ok(())
        }
    );

    create_command!(
        "SaveComment",
        "Save an in-progress review comment",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let command_args = args.args.unwrap_or("".to_string());
            let is_new_comment = command_args == "new".to_string();
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No in-progress review");
                }
                Some(mut review) => {
                    match review.in_progress_comment {
                        Some(mut comment) => {
                            comment.body = get_text_from_current_buffer()?;
                            review.in_progress_comment = None;
                            if is_new_comment {
                                review.add_comment(comment.clone());
                            } else {
                                let (_, idx) = command_args.split_once(" ").unwrap();
                                let idx: usize = str::parse(idx).unwrap();
                                review.comments[idx] = comment.clone();
                            }
                            review.save();
                        }
                        None => {
                            api::err_writeln("No in-progress comment to save.");
                        }
                    };
                }
            }
            Ok(())
        }
    );

    create_command!(
        "ReviewBody",
        "Edit the body text of the review",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(review) => {
                    new_temporary_buffer(Some("SaveReviewBody"))?;
                    set_text_in_buffer(review.body.clone())
                }
            }
        }
    );

    create_command!(
        "SaveReviewBody",
        "Save the buffer contents to the review body",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(mut review) => {
                    review.body = get_text_from_current_buffer()?;
                    review.save();
                    Ok(())
                }
            }
        }
    );

    create_command!(
        "EditComment",
        "Save the buffer contents to the review body",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let (_side, path) = get_current_buffer_path()?;
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(mut review) => {
                    let comment_to_edit = review.get_comment_at_position(
                        path.to_str().unwrap().to_string(),
                        args.line1 as u32,
                    );
                    match comment_to_edit {
                        None => {
                            api::err_writeln("No comment under the cursor.");
                            // TODO: Cleanup of current review
                            Ok(())
                        }
                        Some((idx, comment)) => {
                            // TODO: in progress comment management
                            new_temporary_buffer(Some(&format!("SaveComment existing {}", idx)))?;
                            set_text_in_buffer(comment.body.clone())?;
                            review.in_progress_comment = Some(comment.clone());
                            review.save();
                            Ok(())
                        }
                    }
                }
            }
        }
    );

    create_command!(
        "DeleteComment",
        "Delete the comment under the cursor, if one exists.",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let (_side, path) = get_current_buffer_path()?;
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(mut review) => {
                    let comment_to_delete = review.get_comment_at_position(
                        path.to_str().unwrap().to_string(),
                        args.line1 as u32,
                    );
                    match comment_to_delete {
                        None => {
                            api::err_writeln("No comment under the cursor.");
                            Ok(())
                        }
                        Some((_idx, comment)) => {
                            // TODO: Messy handling of comment deletion
                            review.delete_comment(&comment.clone());
                            review.save();
                            api::out_write("Comment deleted.\n");
                            Ok(())
                        }
                    }
                }
            }
        }
    );

    create_command!(
        "QuickfixAllComments",
        "Load all review comments into the quickfix list",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(review) => {
                    let comments: Array = review
                        .comments
                        .iter()
                        .map(|comment| {
                            Dictionary::from_iter([
                                ("filename", Object::from(comment.path.clone())),
                                ("lnum", Object::from(comment.line)),
                                ("text", Object::from(comment.body.clone())),
                            ])
                        })
                        .collect();
                    api::call_function::<_, i32>("setqflist", (comments, " "))?;
                    Ok(())
                }
            }
        }
    );
    Ok(())
}

fn get_current_review() -> Option<Review> {
    let config = get_config_from_file();
    match config?.active_pr {
        None => None,
        Some(pr_number) => Review::get_review(pr_number),
    }
}

/// Open a new temporary buffer. If `on_save_command` is specified, run the command on BufWritePre
/// on the new buffer.
fn new_temporary_buffer(on_save_command: Option<&str>) -> ApiResult<()> {
    let file = NamedTempFile::new().unwrap();
    api::command(&format!("sp {}", file.path().display()))?;
    api::command("set ft=markdown")?;
    if on_save_command.is_some() {
        api::command(&format!(
            "autocmd BufWritePre <buffer> :{}",
            on_save_command.unwrap()
        ))?;
    }
    Ok(())
}

/// Return a string containing all the text within the current buffer
fn get_text_from_current_buffer() -> ApiResult<String> {
    Ok(api::get_current_buf()
        .get_lines(0..10000000, false)?
        .map(|s| String::from(s.to_string_lossy()))
        .collect::<Vec<String>>()
        .join("\n"))
}

/// Get the relative path in the repository for the file open in the current buffer.
fn get_current_buffer_path() -> ApiResult<(Side, PathBuf)> {
    let repo = Repository::open_from_env().unwrap();
    let workdir = repo.workdir().unwrap();
    let current_buffer = api::get_current_buf();
    let buffer_path = current_buffer.get_name().unwrap();
    let buffer_is_prior_rev = buffer_path.starts_with("fugitive://");
    if buffer_is_prior_rev {
        // Fugitive paths are of the form:
        // fugitive://<hash>/path/to/file
        let re = Regex::new(r".*/.git.*[a-f0-9]{40}/(.*)").unwrap();
        let path = re
            .captures(buffer_path.to_str().unwrap())
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
        return Ok((Side::LEFT, Path::new(path).to_path_buf()));
    }

    match buffer_path.strip_prefix(workdir) {
        Err(e) => {
            api::err_writeln(&format!(
                "Current buffer is not a valid path in the git repository: {}",
                e
            ));
            Err(api::Error::Other(
                "Current buffer not a valid path in the repository".to_string(),
            ))
        }
        Ok(path) => Ok((Side::RIGHT, path.to_path_buf())),
    }
}

#[test]
fn test_environment_detection() {
    let repo = Repository::open_from_env().unwrap();
    let workdir = repo.workdir().unwrap();
    let origin = repo.find_remote("origin").unwrap();
    let remote_url = origin.url().unwrap();
    println!("Workdir: {}", workdir.display());
    println!("{:?}", parse_config_from_url(&remote_url).unwrap());
}

#[oxi::test]
fn test_current_buffer_path() {
    api::command("e src/lib.rs").unwrap();
    assert_eq!(
        get_current_buffer_path(),
        Ok((Side::RIGHT, (Path::new("src/lib.rs").to_path_buf())))
    );
}

#[oxi::test]
fn test_leave_two_comments() {
    vim_reviewer().unwrap();
    api::command("e src/lib.rs").unwrap();
    api::command("StartReview 101").unwrap();
    let opts = CmdOpts::builder().output(true).build();
    let info = CmdInfos::builder()
        .cmd("ReviewComment")
        .range(api::types::CmdRange::Double(25, 28))
        .build();
    // api::command("25,28ReviewComment").unwrap();
    api::cmd(&info, &opts).unwrap();
    api::feedkeys(
        string!("Test.").as_nvim_str(),
        string!("i").as_nvim_str(),
        false,
    );
    api::command("w").unwrap();
    // api::command("50").unwrap();
    // api::command("ReviewComment").unwrap();
    // api::command("wq").unwrap();
}

/// Set the provided text as the contents of the current buffer
fn set_text_in_buffer(text: String) -> ApiResult<()> {
    let mut buffer = api::get_current_buf();
    buffer.set_lines(0..10000000, false, text.split("\n"))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    owner: String,
    repo: String,
    backend: GitBackend,
    #[serde(default)]
    backend_url: Option<String>, // Base URL for the backend (e.g., "https://gitlab.example.com")
    active_pr: Option<u32>,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
pub enum Side {
    RIGHT,
    LEFT,
}

#[derive(Serialize, Deserialize, PartialEq, Clone)]
pub struct Comment {
    body: String,
    line: u32,
    path: String,
    side: Side,
    start_line: Option<u32>,
    start_side: Option<Side>,
}

impl Comment {
    fn new(
        body: String,
        line: u32,
        path: String,
        side: Side,
        start_line: Option<u32>,
        start_side: Option<Side>,
    ) -> Self {
        Comment {
            body,
            line,
            path,
            side,
            start_line,
            start_side,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Review {
    owner: String,
    repo: String,
    backend: GitBackend,
    #[serde(default)]
    backend_url: Option<String>, // Base URL for the backend (e.g., "https://gitlab.example.com")
    pr_number: u32,
    body: String,
    comments: Vec<Comment>,
    in_progress_comment: Option<Comment>,
}

impl Review {
    fn new(
        owner: String,
        repo: String,
        backend: GitBackend,
        backend_url: Option<String>,
        pr_number: u32,
        body: String,
        comments: Vec<Comment>,
    ) -> Self {
        Review {
            owner,
            repo,
            backend,
            backend_url,
            pr_number,
            body,
            comments,
            in_progress_comment: None,
        }
    }

    fn post_url(&self) -> String {
        match self.backend {
            GitBackend::GitHub => {
                format!(
                    "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
                    self.owner, self.repo, self.pr_number
                )
            }
            GitBackend::GitLab => {
                // GitLab uses project ID or URL-encoded path (owner/repo)
                let project_path = format!("{}/{}", self.owner, self.repo);
                let encoded_path = project_path.replace("/", "%2F");
                format!(
                    "https://gitlab.com/api/v4/projects/{}/merge_requests/{}/discussions",
                    encoded_path, self.pr_number
                )
            }
        }
    }

    pub fn publish(&self, token: String) -> Result<reqwest::blocking::Response, reqwest::Error> {
        match self.backend {
            GitBackend::GitHub => self.publish_github(token),
            GitBackend::GitLab => self.publish_gitlab(token),
        }
    }

    fn publish_github(&self, token: String) -> Result<reqwest::blocking::Response, reqwest::Error> {
        let client = reqwest::blocking::Client::new();
        fn header_map(token: String) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/vnd.github+json"),
            );
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("token {}", token)).unwrap(),
            );
            headers.insert(USER_AGENT, HeaderValue::from_static("vim-reviewer"));
            headers
        }
        client
            .post(self.post_url())
            .json(&self)
            .headers(header_map(token))
            .send()
    }

    fn publish_gitlab(&self, token: String) -> Result<reqwest::blocking::Response, reqwest::Error> {
        let client = reqwest::blocking::Client::new();

        fn header_map(token: String) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
            );
            headers.insert(USER_AGENT, HeaderValue::from_static("vim-reviewer"));
            headers
        }

        // Use the backend_url from config, or default to gitlab.com
        let base_url = self
            .backend_url
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("https://gitlab.com");

        let encoded_project = format!("{}/{}", self.owner, self.repo).replace("/", "%2F");
        let mut last_response: Option<reqwest::blocking::Response> = None;

        // GitLab API doesn't have a direct equivalent to GitHub's review API.
        // We need to create individual discussion threads for each comment.
        // First, create a general note with the review body if it exists
        if !self.body.is_empty() {
            let body_payload = serde_json::json!({
                "body": self.body,
            });
            let mr_notes_url = format!(
                "{}/api/v4/projects/{}/merge_requests/{}/notes",
                base_url, encoded_project, self.pr_number
            );
            last_response = Some(
                client
                    .post(&mr_notes_url)
                    .json(&body_payload)
                    .headers(header_map(token.clone()))
                    .send()?,
            );
        }

        // Fetch MR details to get the required SHAs for diff comments
        let mr_url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            base_url, encoded_project, self.pr_number
        );
        let mr_response = client
            .get(&mr_url)
            .headers(header_map(token.clone()))
            .send()?;

        // Parse the MR response to get the SHAs
        let mr_data: serde_json::Value = match mr_response.json() {
            Ok(data) => data,
            Err(e) => {
                api::err_writeln(&format!("Failed to parse MR data: {}", e));
                return Err(e);
            }
        };

        let base_sha = mr_data["diff_refs"]["base_sha"].as_str().unwrap_or("");
        let start_sha = mr_data["diff_refs"]["start_sha"].as_str().unwrap_or("");
        let head_sha = mr_data["diff_refs"]["head_sha"].as_str().unwrap_or("");

        // Now create discussion threads for each comment
        for comment in &self.comments {
            // For multi-line comments, use start_line and line (end line)
            // For single-line comments, start_line will be line-1, so use line for both
            let is_multi_line = comment.start_line.is_some() 
                && comment.start_line.unwrap() != comment.line 
                && comment.start_line.unwrap() != comment.line - 1;
            
            let (line_start, line_end) = if is_multi_line {
                (comment.start_line.unwrap(), comment.line)
            } else {
                (comment.line, comment.line)
            };

            let new_line = if comment.side == Side::RIGHT {
                serde_json::Value::from(line_start)
            } else {
                serde_json::Value::Null
            };
            let old_line = if comment.side == Side::LEFT {
                serde_json::Value::from(line_start)
            } else {
                serde_json::Value::Null
            };

            // If path is a windows path, convert to unix
            let comment_path = if cfg!(windows) {
                comment.path.replace("\\", "/")
            } else {
                comment.path.clone()
            };

            let (new_path, old_path) = if comment.side == Side::RIGHT {
                (serde_json::Value::from(comment_path), serde_json::Value::Null)
            } else {
                (serde_json::Value::Null, serde_json::Value::from(comment_path))
            };

            // Build position object with optional line_range for multi-line comments
            let mut position = serde_json::json!({
                "position_type": "text",
                "base_sha": base_sha,
                "start_sha": start_sha,
                "head_sha": head_sha,
                "new_path": new_path,
                "old_path": old_path,
                "new_line": new_line,
                "old_line": old_line,
            });

            // Add line_range for multi-line comments
            if is_multi_line {
                let line_range = if comment.side == Side::RIGHT {
                    serde_json::json!({
                        "start": {
                            "line_code": format!("{}_{}", comment.path, line_start),
                            "type": "new",
                        },
                        "end": {
                            "line_code": format!("{}_{}", comment.path, line_end),
                            "type": "new",
                        }
                    })
                } else {
                    serde_json::json!({
                        "start": {
                            "line_code": format!("{}_{}", comment.path, line_start),
                            "type": "old",
                        },
                        "end": {
                            "line_code": format!("{}_{}", comment.path, line_end),
                            "type": "old",
                        }
                    })
                };
                position["line_range"] = line_range;
            }

            let discussion_payload = serde_json::json!({
                "body": comment.body,
                "position": position
            });

            api::out_write(string!( "Posting payload {:?} to GitLab\n", discussion_payload));

            let url = format!(
                "{}/api/v4/projects/{}/merge_requests/{}/discussions",
                base_url, encoded_project, self.pr_number
            );

            last_response = Some(
                client
                    .post(&url)
                    .json(&discussion_payload)
                    .headers(header_map(token.clone()))
                    .send()?,
            );
        }

        // Return the last response, or fetch the MR if no comments were posted
        match last_response {
            Some(response) => Ok(response),
            None => {
                // No comments or body, just verify the MR exists
                let mr_url = format!(
                    "{}/api/v4/projects/{}/merge_requests/{}",
                    base_url, encoded_project, self.pr_number
                );
                client.get(&mr_url).headers(header_map(token)).send()
            }
        }
    }

    pub fn add_comment(&mut self, comment: Comment) {
        self.comments.push(comment);
    }

    pub fn set_body(&mut self, body: String) {
        self.body = body;
    }

    pub fn save(&self) {
        let review_file_path = get_review_file_path(self.pr_number);
        let mut file = match File::create(&review_file_path) {
            Err(err) => {
                api::err_writeln(&format!(
                    "Error creating {}: {}",
                    review_file_path.display(),
                    err
                ));
                return;
            }
            Ok(file) => file,
        };
        file.write_all(&serde_json::to_string(&self).unwrap().as_bytes())
            .unwrap();
    }

    /// Return the first comment in this review whose span contains the requested file path and
    /// line.
    pub fn get_comment_at_position(&self, path: String, line: u32) -> Option<(usize, &Comment)> {
        let eligible_comments: Vec<(usize, &Comment)> = self
            .comments
            .iter()
            .enumerate()
            .filter(|(_idx, comment)| {
                return comment.path == path
                    && (comment.line == line
                        || (comment.start_line.is_some()
                            && comment.start_line.unwrap() <= line
                            && comment.line >= line));
            })
            .collect();
        if eligible_comments.len() > 0 {
            Some(eligible_comments[0])
        } else {
            None
        }
    }

    pub fn delete_comment(&mut self, comment: &Comment) {
        // TODO: Better error handling here
        let (idx, _matched_comment) = self
            .comments
            .iter()
            .enumerate()
            .find(|(_idx, c)| *c == comment)
            .unwrap();
        self.comments.remove(idx);
    }

    pub fn get_review(pr_number: u32) -> Option<Self> {
        let review_file_path = get_review_file_path(pr_number);
        if review_file_path.exists() {
            let mut review_string = String::new();
            match File::open(review_file_path) {
                Err(e) => {
                    api::err_writeln(&format!("Could not open review file: {}", e));
                    return None;
                }
                Ok(mut file) => {
                    file.read_to_string(&mut review_string).unwrap();
                }
            }
            Some(serde_json::from_str(&review_string).unwrap())
        } else {
            // New review
            match get_config_from_file() {
                None => {
                    api::err_writeln("Could not read configuration file.");
                    return None;
                }
                Some(config) => Some(Review::new(
                    config.owner.to_string(),
                    config.repo.to_string(),
                    config.backend.clone(),
                    config.backend_url.clone(),
                    pr_number,
                    "".to_string(),
                    vec![],
                )),
            }
        }
    }
}

fn get_review_directory() -> PathBuf {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--git-dir")
        .output()
        .expect("git command failed");
    let git_output = String::from_utf8(output.stdout).unwrap();
    let git_dir = Path::new(git_output.trim());
    let review_dir = git_dir.join(Path::new("reviews"));
    std::fs::create_dir_all(&review_dir).unwrap();
    return review_dir;
}

fn get_review_file_path(pr_number: u32) -> PathBuf {
    get_review_directory().join(Path::new(&format!("{}-review.json", pr_number)))
}

fn get_config_file_path() -> PathBuf {
    let review_directory = get_review_directory();
    review_directory.join("config.json")
}

fn get_config_from_file() -> Option<Config> {
    let config_file_path = get_config_file_path();
    let mut config_string = String::new();
    match File::open(&config_file_path) {
        Err(e) => {
            api::err_writeln(&format!(
                "Could not open configuration file {}: {}",
                config_file_path.display(),
                e
            ));
            return None;
        }
        Ok(mut file) => {
            file.read_to_string(&mut config_string).unwrap();
        }
    }
    Some(serde_json::from_str(&config_string).unwrap())
}

pub fn update_configuration(config: Config) {
    let config_file_path = get_config_file_path();
    let mut file = match File::create(&config_file_path) {
        Err(err) => {
            api::err_writeln(&format!(
                "Error creating {}: {}",
                config_file_path.display(),
                err
            ));
            return;
        }
        Ok(file) => file,
    };
    file.write_all(&serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
}



