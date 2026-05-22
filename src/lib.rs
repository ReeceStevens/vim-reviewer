extern crate git2;
extern crate nvim_oxi;
extern crate regex;
extern crate reqwest;
extern crate serde;
extern crate sha1;
extern crate tempfile;
extern crate toml;

use git2::Repository;
use nvim_oxi::api;
use nvim_oxi::api::opts::CreateCommandOpts;
use nvim_oxi::api::types::{CommandArgs, CommandNArgs, CommandRange};
use nvim_oxi::string;
use nvim_oxi::{self as oxi, Array, Dictionary, Object};
use regex::Regex;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tempfile::NamedTempFile;

use std::cell::RefCell;
use std::collections::HashSet;
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

/// Action associated with a line in the status buffer.
#[derive(Clone, Debug)]
enum StatusLineAction {
    None,
    OpenFile(String),
    JumpToComment(String, u32),
    ToggleComment(usize),
}

thread_local! {
    static EXPANDED_COMMENTS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    static STATUS_BUFFER_HANDLE: RefCell<Option<nvim_oxi::api::Buffer>> = const { RefCell::new(None) };
    static STATUS_LINE_ACTIONS: RefCell<Vec<StatusLineAction>> = const { RefCell::new(Vec::new()) };
}

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
            base_branch: None,
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
        Err(_) => return Ok(()),
    };
    let remote_url = match repo.find_remote("origin") {
        Ok(remote) => match remote.url() {
            Some(url) => url.to_string(),
            None => return Ok(()),
        },
        Err(_) => return Ok(()),
    };
    let (owner, repo_name, backend) = match parse_config_from_url(&remote_url) {
        Ok(results) => results,
        Err(_) => return Ok(()),
    };

    update_configuration(Config {
        owner,
        repo: repo_name,
        backend,
        backend_url: None,
        active_pr: None,
        base_branch: None,
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
        |_args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                None => Ok(()),
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
                                .filter(|comment| comment.path == buffer_path.to_str().unwrap())
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
                                        sign_idx, line, handle,
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
        "Start a review: StartReview <pr_number> [base_branch]",
        CommandNArgs::OneOrMore,
        |args: CommandArgs| -> ApiResult<()> {
            match get_config_from_file() {
                None => {
                    api::err_writeln("Could not read configuration file.");
                    Ok(())
                }
                Some(mut config) => {
                    let raw = args.args.unwrap_or_default();
                    let parts: Vec<&str> = raw.split_whitespace().collect();
                    if parts.is_empty() {
                        api::err_writeln("Usage: StartReview <pr_number> [base_branch]");
                        return Ok(());
                    }
                    let pr_number = match str::parse::<u32>(parts[0]) {
                        Ok(n) => n,
                        Err(_) => {
                            api::err_writeln("Invalid PR number.");
                            return Ok(());
                        }
                    };
                    config.active_pr = Some(pr_number);

                    let base_branch = if parts.len() > 1 {
                        parts[1].to_string()
                    } else {
                        // Try to detect default branch
                        detect_default_branch().unwrap_or_else(|| "main".to_string())
                    };
                    config.base_branch = Some(base_branch);
                    update_configuration(config);

                    // Optionally try API fetch for enrichment
                    if let Some(config) = get_config_from_file()
                        && let Some(pr_info) = fetch_pr_info_from_api(&config, pr_number)
                    {
                        // Cache the enriched info
                        if let Ok(json) = serde_json::to_string(&pr_info)
                            && let Ok(mut file) = File::create(get_pr_info_cache_path(pr_number))
                        {
                            let _ = file.write_all(json.as_bytes());
                        }
                    }

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
            let is_new_comment = command_args == "new";
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
        "ShowComment",
        "Display the comment under the cursor in a floating hover window.",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> ApiResult<()> {
            let (_side, path) = get_current_buffer_path()?;
            let review = get_current_review();
            match review {
                None => {
                    api::err_writeln("No review is currently active.");
                    Ok(())
                }
                Some(review) => {
                    let comment_to_show = review.get_comment_at_position(
                        path.to_str().unwrap().to_string(),
                        args.line1 as u32,
                    );
                    match comment_to_show {
                        None => {
                            api::out_write("No comment on selected line\n");
                            Ok(())
                        }
                        Some((_idx, comment)) => show_comment_hover(&comment.body),
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
        |_args: CommandArgs| -> ApiResult<()> {
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

    // Status window commands
    create_command!(
        "ReviewStatus",
        "Open or focus the review status window",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let config = match get_config_from_file() {
                Some(c) => c,
                None => {
                    api::err_writeln("No configuration file found. Run StartReview first.");
                    return Ok(());
                }
            };
            let pr_number = match config.active_pr {
                Some(n) => n,
                None => {
                    api::err_writeln("No active review. Run StartReview first.");
                    return Ok(());
                }
            };
            open_status_buffer(pr_number)?;
            Ok(())
        }
    );

    create_command!(
        "ReviewStatusRefresh",
        "Force refresh the review status window",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            // Invalidate PrInfo cache so files are re-computed from git
            if let Some(config) = get_config_from_file()
                && let Some(pr_number) = config.active_pr
            {
                let cache_path = get_pr_info_cache_path(pr_number);
                let _ = std::fs::remove_file(&cache_path);
            }
            refresh_status_buffer()?;
            Ok(())
        }
    );

    create_command!(
        "ReviewStatusEnter",
        "Activate the item under the cursor in the status window",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let cursor = api::get_current_win().get_cursor()?;
            let line_idx = (cursor.0 as usize).saturating_sub(1); // 1-indexed to 0-indexed

            let action = STATUS_LINE_ACTIONS.with(|a| {
                let actions = a.borrow();
                actions.get(line_idx).cloned()
            });

            let config = get_config_from_file();
            let base_branch = config
                .as_ref()
                .and_then(|c| c.base_branch.as_deref())
                .unwrap_or("main");

            match action {
                Some(StatusLineAction::OpenFile(path)) => {
                    // Move to window below, open file, run diff
                    api::command("wincmd j")?;
                    api::command(&format!("edit {}", path))?;
                    let diff_cmd = format!("Gvdiffsplit origin/{}", base_branch);
                    if api::command(&diff_cmd).is_err() {
                        // Fallback: just open the file without diff
                        api::err_writeln(
                            "Could not open fugitive diff. Is vim-fugitive installed?",
                        );
                    }
                }
                Some(StatusLineAction::JumpToComment(path, line)) => {
                    api::command("wincmd j")?;
                    api::command(&format!("edit {}", path))?;
                    let diff_cmd = format!("Gvdiffsplit origin/{}", base_branch);
                    let _ = api::command(&diff_cmd);
                    api::command(&format!("{}", line))?;
                }
                Some(StatusLineAction::ToggleComment(idx)) => {
                    EXPANDED_COMMENTS.with(|e| {
                        let mut set = e.borrow_mut();
                        if set.contains(&idx) {
                            set.remove(&idx);
                        } else {
                            set.insert(idx);
                        }
                    });
                    let saved_cursor = api::get_current_win().get_cursor()?;
                    refresh_status_buffer()?;
                    let _ = api::get_current_win().set_cursor(saved_cursor.0, saved_cursor.1);
                }
                _ => {}
            }
            Ok(())
        }
    );

    create_command!(
        "ReviewStatusTab",
        "Toggle expand/collapse of comment under cursor in status window",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let cursor = api::get_current_win().get_cursor()?;
            let line_idx = (cursor.0 as usize).saturating_sub(1);

            let action = STATUS_LINE_ACTIONS.with(|a| {
                let actions = a.borrow();
                actions.get(line_idx).cloned()
            });

            if let Some(StatusLineAction::ToggleComment(idx)) = action {
                EXPANDED_COMMENTS.with(|e| {
                    let mut set = e.borrow_mut();
                    if set.contains(&idx) {
                        set.remove(&idx);
                    } else {
                        set.insert(idx);
                    }
                });
                let saved_cursor = api::get_current_win().get_cursor()?;
                refresh_status_buffer()?;
                let _ = api::get_current_win().set_cursor(saved_cursor.0, saved_cursor.1);
            }
            Ok(())
        }
    );

    create_command!(
        "ReviewStatusViewed",
        "Toggle viewed status of file under cursor in status window",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let cursor = api::get_current_win().get_cursor()?;
            let line_idx = (cursor.0 as usize).saturating_sub(1);

            let action = STATUS_LINE_ACTIONS.with(|a| {
                let actions = a.borrow();
                actions.get(line_idx).cloned()
            });

            if let Some(StatusLineAction::OpenFile(path)) = action {
                let config = get_config_from_file();
                if let Some(pr_number) = config.and_then(|c| c.active_pr)
                    && let Some(mut review) = Review::get_review(pr_number)
                {
                    review.toggle_viewed(&path);
                    review.save();
                }
                let saved_cursor = api::get_current_win().get_cursor()?;
                refresh_status_buffer()?;
                let _ = api::get_current_win().set_cursor(saved_cursor.0, saved_cursor.1);
            }
            Ok(())
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
    if let Some(cmd) = on_save_command {
        api::command(&format!("autocmd BufWritePre <buffer> :{}", cmd))?;
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

/// Detect the default branch by checking if main or master exists.
fn detect_default_branch() -> Option<String> {
    let repo = Repository::open_from_env().ok()?;
    for branch_name in &["main", "master"] {
        if repo
            .revparse_single(&format!("origin/{}", branch_name))
            .is_ok()
            || repo.revparse_single(branch_name).is_ok()
        {
            return Some(branch_name.to_string());
        }
    }
    None
}

/// Get files changed between base_branch and HEAD using local git operations.
fn get_files_changed(base_branch: &str) -> Result<Vec<FileChange>, String> {
    let repo = Repository::open_from_env().map_err(|e| format!("Failed to open repo: {}", e))?;

    // Try origin/{base} first, then {base} directly
    let base_ref = repo
        .revparse_single(&format!("origin/{}", base_branch))
        .or_else(|_| repo.revparse_single(base_branch))
        .map_err(|e| format!("Failed to resolve base branch '{}': {}", base_branch, e))?;

    let head_ref = repo
        .revparse_single("HEAD")
        .map_err(|e| format!("Failed to resolve HEAD: {}", e))?;

    // Compute merge-base
    let merge_base = repo
        .merge_base(base_ref.id(), head_ref.id())
        .map_err(|e| format!("Failed to find merge-base: {}", e))?;

    let merge_base_commit = repo
        .find_commit(merge_base)
        .map_err(|e| format!("Failed to find merge-base commit: {}", e))?;
    let merge_base_tree = merge_base_commit
        .tree()
        .map_err(|e| format!("Failed to get merge-base tree: {}", e))?;

    let head_commit = head_ref
        .peel_to_commit()
        .map_err(|e| format!("Failed to peel HEAD to commit: {}", e))?;
    let head_tree = head_commit
        .tree()
        .map_err(|e| format!("Failed to get HEAD tree: {}", e))?;

    let diff = repo
        .diff_tree_to_tree(Some(&merge_base_tree), Some(&head_tree), None)
        .map_err(|e| format!("Failed to create diff: {}", e))?;

    let mut files: Vec<FileChange> = Vec::new();

    for idx in 0..diff.deltas().len() {
        let delta = diff.get_delta(idx).unwrap();
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified => "M",
            git2::Delta::Renamed => "R",
            git2::Delta::Copied => "C",
            _ => "?",
        }
        .to_string();

        // Count additions and deletions from the patch
        let mut additions: u32 = 0;
        let mut deletions: u32 = 0;

        if let Ok(patch) = git2::Patch::from_diff(&diff, idx)
            && let Some(ref patch) = patch
        {
            let (_, adds, dels) = patch.line_stats().unwrap_or((0, 0, 0));
            additions = adds as u32;
            deletions = dels as u32;
        }

        files.push(FileChange {
            path,
            additions,
            deletions,
            status,
        });
    }

    Ok(files)
}

fn get_pr_info_cache_path(pr_number: u32) -> PathBuf {
    get_review_directory().join(format!("{}-prinfo.json", pr_number))
}

/// Get or build PrInfo, using disk cache when available.
fn get_or_build_pr_info(config: &Config, pr_number: u32) -> Option<PrInfo> {
    let cache_path = get_pr_info_cache_path(pr_number);
    let base_branch = config.base_branch.as_deref().unwrap_or("main").to_string();

    // Try loading from cache
    if cache_path.exists()
        && let Ok(mut file) = File::open(&cache_path)
    {
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_ok()
            && let Ok(cached) = serde_json::from_str::<PrInfo>(&contents)
            && cached.base_branch == base_branch
        {
            return Some(cached);
        }
    }

    // Build locally
    let files_changed = match get_files_changed(&base_branch) {
        Ok(files) => files,
        Err(e) => {
            api::err_writeln(&format!("Failed to get files changed: {}", e));
            Vec::new()
        }
    };

    let pr_info = PrInfo {
        pr_number,
        title: None,
        base_branch,
        head_branch: None,
        files_changed,
    };

    // Cache to disk
    if let Ok(json) = serde_json::to_string(&pr_info)
        && let Ok(mut file) = File::create(&cache_path)
    {
        let _ = file.write_all(json.as_bytes());
    }

    Some(pr_info)
}

/// Optionally fetch PR/MR info from the API for enrichment (title, head_branch).
/// Returns None on any failure — this is never required.
fn fetch_pr_info_from_api(config: &Config, pr_number: u32) -> Option<PrInfo> {
    let base_branch = config.base_branch.as_deref().unwrap_or("main").to_string();

    let (token_var, _) = match config.backend {
        GitBackend::GitHub => ("GH_REVIEW_API_TOKEN", "GitHub"),
        GitBackend::GitLab => ("GITLAB_TOKEN", "GitLab"),
    };
    let token = env::var(token_var).ok()?;

    let client = reqwest::blocking::Client::new();

    match config.backend {
        GitBackend::GitHub => {
            let url = format!(
                "https://api.github.com/repos/{}/{}/pulls/{}",
                config.owner, config.repo, pr_number
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/vnd.github+json"),
            );
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("token {}", token)).ok()?,
            );
            headers.insert(USER_AGENT, HeaderValue::from_static("vim-reviewer"));

            let resp = client.get(&url).headers(headers).send().ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let data: serde_json::Value = resp.json().ok()?;
            let title = data["title"].as_str().map(|s| s.to_string());
            let head_branch = data["head"]["ref"].as_str().map(|s| s.to_string());
            let api_base = data["base"]["ref"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or(base_branch);

            let files_changed = get_files_changed(&api_base).unwrap_or_default();

            Some(PrInfo {
                pr_number,
                title,
                base_branch: api_base,
                head_branch,
                files_changed,
            })
        }
        GitBackend::GitLab => {
            let base_url = config
                .backend_url
                .as_deref()
                .unwrap_or("https://gitlab.com");
            let encoded_project = format!("{}/{}", config.owner, config.repo).replace("/", "%2F");
            let url = format!(
                "{}/api/v4/projects/{}/merge_requests/{}",
                base_url, encoded_project, pr_number
            );

            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", token)).ok()?,
            );
            headers.insert(USER_AGENT, HeaderValue::from_static("vim-reviewer"));

            let resp = client.get(&url).headers(headers).send().ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let data: serde_json::Value = resp.json().ok()?;
            let title = data["title"].as_str().map(|s| s.to_string());
            let head_branch = data["source_branch"].as_str().map(|s| s.to_string());
            let api_base = data["target_branch"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or(base_branch);

            let files_changed = get_files_changed(&api_base).unwrap_or_default();

            Some(PrInfo {
                pr_number,
                title,
                base_branch: api_base,
                head_branch,
                files_changed,
            })
        }
    }
}

/// Format a status char for display (e.g. "M" -> "M", "A" -> "A").
fn format_file_status_char(status: &str) -> &str {
    match status {
        "A" => "A",
        "D" => "D",
        "M" => "M",
        "R" => "R",
        "C" => "C",
        _ => "?",
    }
}

/// Build the status buffer lines and their associated actions.
fn build_status_lines(
    pr_number: u32,
    pr_info: &PrInfo,
    review: Option<&Review>,
) -> (Vec<String>, Vec<StatusLineAction>) {
    let mut lines: Vec<String> = Vec::new();
    let mut actions: Vec<StatusLineAction> = Vec::new();

    // Header
    let title_str = match &pr_info.title {
        Some(t) => format!(" - {}", t),
        None => String::new(),
    };
    lines.push(format!("== Review #{}{} ==", pr_number, title_str));
    actions.push(StatusLineAction::None);

    lines.push(String::new());
    actions.push(StatusLineAction::None);

    lines.push(format!("Base: {}", pr_info.base_branch));
    actions.push(StatusLineAction::None);

    if let Some(ref head) = pr_info.head_branch {
        lines.push(format!("Head: {}", head));
        actions.push(StatusLineAction::None);
    }

    lines.push("Status: Review in progress".to_string());
    actions.push(StatusLineAction::None);

    lines.push(String::new());
    actions.push(StatusLineAction::None);

    // Files changed section
    let viewed_count = review
        .map(|r| {
            pr_info
                .files_changed
                .iter()
                .filter(|fc| r.viewed_files.contains(&fc.path))
                .count()
        })
        .unwrap_or(0);
    lines.push(format!(
        "Files changed ({}/{}):",
        viewed_count,
        pr_info.files_changed.len()
    ));
    actions.push(StatusLineAction::None);

    let separator = "\u{2500}".repeat(50);
    lines.push(separator.clone());
    actions.push(StatusLineAction::None);

    let comments = review.map(|r| &r.comments).cloned().unwrap_or_default();

    let expanded = EXPANDED_COMMENTS.with(|e| e.borrow().clone());

    let mut comment_global_idx: usize = 0;

    for fc in &pr_info.files_changed {
        let viewed = review.is_some_and(|r| r.viewed_files.contains(&fc.path));
        let checkbox = if viewed { "[x]" } else { "[ ]" };
        let stat_display = format!("+{} -{}", fc.additions, fc.deletions);
        lines.push(format!(
            "  {} {} {:<40} | {}",
            checkbox,
            format_file_status_char(&fc.status),
            fc.path,
            stat_display
        ));
        actions.push(StatusLineAction::OpenFile(fc.path.clone()));

        // Find comments for this file
        let file_comments: Vec<(usize, &Comment)> = comments
            .iter()
            .enumerate()
            .filter(|(_, c)| c.path == fc.path)
            .collect();

        for (_review_comment_idx, comment) in &file_comments {
            let line_display = match comment.start_line {
                Some(sl) if sl != comment.line => format!("L{}-{}", sl, comment.line),
                _ => format!("L{}", comment.line),
            };

            if expanded.contains(&comment_global_idx) {
                // Show full comment body (indented)
                let header = format!("    [-] {} :", line_display);
                lines.push(header);
                actions.push(StatusLineAction::ToggleComment(comment_global_idx));
                for body_line in comment.body.lines() {
                    lines.push(format!("         {}", body_line));
                    actions.push(StatusLineAction::JumpToComment(
                        fc.path.clone(),
                        comment.line,
                    ));
                }
            } else {
                // Collapsed: first 50 chars
                let preview: String = comment
                    .body
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .replace('\n', " ");
                let suffix = if comment.body.len() > 50 { "..." } else { "" };
                lines.push(format!("    [+] {} : {}{}", line_display, preview, suffix));
                actions.push(StatusLineAction::ToggleComment(comment_global_idx));
            }
            comment_global_idx += 1;
        }
    }

    // Separator before body
    lines.push(String::new());
    actions.push(StatusLineAction::None);
    lines.push(separator);
    actions.push(StatusLineAction::None);

    // Review body
    if let Some(review) = review
        && !review.body.is_empty()
    {
        lines.push("Review body:".to_string());
        actions.push(StatusLineAction::None);
        for body_line in review.body.lines() {
            lines.push(format!("  {}", body_line));
            actions.push(StatusLineAction::None);
        }
    }

    (lines, actions)
}

/// Install buffer-local keymaps for the status buffer.
fn install_status_keymaps() -> ApiResult<()> {
    let keymaps = [
        ("n", "<CR>", ":ReviewStatusEnter<CR>"),
        ("n", "<Tab>", ":ReviewStatusTab<CR>"),
        ("n", "q", ":bwipeout<CR>"),
        ("n", "R", ":ReviewStatusRefresh<CR>"),
        ("n", "C", ":ReviewStatusViewed<CR>"),
    ];
    for (_mode, lhs, rhs) in &keymaps {
        api::command(&format!("nnoremap <buffer> <silent> {} {}", lhs, rhs))?;
    }
    Ok(())
}

/// Apply syntax highlighting rules to the current status buffer.
fn install_status_syntax() -> ApiResult<()> {
    let rules = [
        // Title line: == Review #19 - Fix the widget ==
        r#"syn match reviewStatusTitle /^== .\+ ==$/"#,
        // Key-value headers: Base: main, Head: fix-widget, Status: ...
        r#"syn match reviewStatusHeader /^\(Base\|Head\|Status\):/ nextgroup=reviewStatusBranch skipwhite"#,
        r#"syn match reviewStatusBranch /.*/ contained"#,
        // Section headings: Files changed (1/3):, Review body:
        r#"syn match reviewStatusHeading /^Files changed\ze\s\+(\d\+\/\d\+):/ nextgroup=reviewStatusCount skipwhite"#,
        r#"syn match reviewStatusHeading /^Review body:/"#,
        r#"syn match reviewStatusCount /(\d\+\/\d\+)/hs=s+1,he=e-1 contained"#,
        // Separator: ─────────
        r#"syn match reviewStatusSeparator /^─\+$/"#,
        // File entry: "  [x] M src/lib.rs                | +45 -12"
        r#"syn match reviewStatusFile /^\s\+\[.\]\s[MADRC?]\s.\+|/ contains=reviewStatusViewed,reviewStatusUnviewed,reviewStatusModifier,reviewStatusPath,reviewStatusPipe"#,
        // Viewed/unviewed checkbox
        r#"syn match reviewStatusViewed /\[x\]/ contained"#,
        r#"syn match reviewStatusUnviewed /\[ \]/ contained"#,
        r#"syn match reviewStatusModifier /[MADRC?]/ contained"#,
        r#"syn match reviewStatusPath /[MADRC?]\@<=\s\+\S\+/ contained"#,
        r#"syn match reviewStatusPipe /|/ contained"#,
        // Diff stats: +45, -12
        r#"syn match reviewStatusAdd /|\s\+\zs+\d\+/ containedin=reviewStatusFile"#,
        r#"syn match reviewStatusDelete /|\s\++\d\+\s\+\zs-\d\+/ containedin=reviewStatusFile"#,
        // Comment toggle and line refs
        r#"syn match reviewStatusToggle /\[[-+]\]/"#,
        r#"syn match reviewStatusLineRef /L\d\+\(-\d\+\)\=/"#,
        // Comment body (9-space indent)
        r#"syn match reviewStatusCommentBody /^\s\{9\}.\+$/"#,
        // Highlight links
        "hi def link reviewStatusTitle Title",
        "hi def link reviewStatusHeader Label",
        "hi def link reviewStatusBranch Function",
        "hi def link reviewStatusHeading PreProc",
        "hi def link reviewStatusCount Number",
        "hi def link reviewStatusSeparator Comment",
        "hi def link reviewStatusModifier Type",
        "hi def link reviewStatusPath Directory",
        "hi def link reviewStatusPipe Comment",
        "hi def link reviewStatusAdd diffAdded",
        "hi def link reviewStatusDelete diffRemoved",
        "hi def link reviewStatusViewed diffAdded",
        "hi def link reviewStatusUnviewed Comment",
        "hi def link reviewStatusToggle Special",
        "hi def link reviewStatusLineRef Number",
        "hi def link reviewStatusCommentBody Comment",
    ];
    for rule in &rules {
        api::command(rule)?;
    }
    Ok(())
}

/// Open the status buffer (or focus it if already open).
fn open_status_buffer(pr_number: u32) -> ApiResult<()> {
    // Check if we already have a status buffer open
    let existing = STATUS_BUFFER_HANDLE.with(|h| h.borrow().clone());
    if let Some(ref buf) = existing {
        // Check if the buffer is still valid
        let obj: oxi::Object = buf.into();
        let handle = unsafe { obj.as_integer_unchecked() };
        if api::call_function::<_, i32>("bufexists", (handle,))? != 0 {
            // Focus the existing buffer
            api::command(&format!("sbuffer {}", handle))?;
            refresh_status_buffer()?;
            return Ok(());
        }
    }

    // Create a new scratch buffer
    let buf = api::create_buf(false, true)
        .map_err(|_| api::Error::Other("Failed to create status buffer".to_string()))?;

    let obj: oxi::Object = (&buf).into();
    let handle = unsafe { obj.as_integer_unchecked() };

    // Open as horizontal split at top
    api::command(&format!("topleft sbuffer {}", handle))?;

    // Set buffer options
    api::command(
        "setlocal buftype=nofile bufhidden=wipe noswapfile nomodifiable nonumber norelativenumber signcolumn=no",
    )?;

    // Set buffer name (escape # to prevent neovim command-line expansion)
    api::command(&format!("file [Review\\ \\#{}]", pr_number))?;

    // Store the handle
    STATUS_BUFFER_HANDLE.with(|h| {
        *h.borrow_mut() = Some(buf);
    });

    // Clear expanded comments
    EXPANDED_COMMENTS.with(|e| e.borrow_mut().clear());

    // Render content, install keymaps, and apply syntax highlighting
    refresh_status_buffer()?;
    install_status_keymaps()?;
    install_status_syntax()?;

    Ok(())
}

/// Refresh the status buffer content by re-reading review data.
fn refresh_status_buffer() -> ApiResult<()> {
    let buf = STATUS_BUFFER_HANDLE.with(|h| h.borrow().clone());
    let mut buf = match buf {
        Some(b) => b,
        None => return Ok(()),
    };

    let config = match get_config_from_file() {
        Some(c) => c,
        None => return Ok(()),
    };

    let pr_number = match config.active_pr {
        Some(n) => n,
        None => return Ok(()),
    };

    let pr_info = match get_or_build_pr_info(&config, pr_number) {
        Some(info) => info,
        None => return Ok(()),
    };

    let review = Review::get_review(pr_number);
    let (lines, new_actions) = build_status_lines(pr_number, &pr_info, review.as_ref());

    STATUS_LINE_ACTIONS.with(|a| {
        *a.borrow_mut() = new_actions;
    });

    // Make buffer modifiable, write lines, make nomodifiable
    api::command("setlocal modifiable")?;
    let line_strs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    buf.set_lines(.., false, line_strs)?;
    api::command("setlocal nomodifiable")?;

    Ok(())
}

#[test]
fn test_build_status_lines_basic() {
    let pr_info = PrInfo {
        pr_number: 42,
        title: Some("Fix the widget".to_string()),
        base_branch: "main".to_string(),
        head_branch: Some("fix-widget".to_string()),
        files_changed: vec![
            FileChange {
                path: "src/lib.rs".to_string(),
                additions: 10,
                deletions: 3,
                status: "M".to_string(),
            },
            FileChange {
                path: "src/new.rs".to_string(),
                additions: 50,
                deletions: 0,
                status: "A".to_string(),
            },
        ],
    };

    let (lines, actions) = build_status_lines(42, &pr_info, None);

    // Header
    assert!(lines[0].contains("Review #42"));
    assert!(lines[0].contains("Fix the widget"));

    // Base and head
    assert!(lines.iter().any(|l| l.contains("Base: main")));
    assert!(lines.iter().any(|l| l.contains("Head: fix-widget")));

    // Files
    assert!(lines.iter().any(|l| l.contains("Files changed (0/2):")));
    assert!(lines.iter().any(|l| l.contains("M src/lib.rs")));
    assert!(lines.iter().any(|l| l.contains("A src/new.rs")));
    assert!(lines.iter().any(|l| l.contains("+10 -3")));

    // Actions length matches lines length
    assert_eq!(lines.len(), actions.len());
}

#[test]
fn test_build_status_lines_with_comments() {
    let pr_info = PrInfo {
        pr_number: 7,
        title: None,
        base_branch: "main".to_string(),
        head_branch: None,
        files_changed: vec![FileChange {
            path: "src/lib.rs".to_string(),
            additions: 5,
            deletions: 2,
            status: "M".to_string(),
        }],
    };

    let review = Review {
        owner: "test".to_string(),
        repo: "repo".to_string(),
        backend: GitBackend::GitHub,
        backend_url: None,
        pr_number: 7,
        body: "Looks good overall".to_string(),
        comments: vec![Comment::new(
            "Please rename this variable".to_string(),
            42,
            "src/lib.rs".to_string(),
            Side::RIGHT,
            None,
            None,
        )],
        in_progress_comment: None,
        viewed_files: HashSet::new(),
    };

    let (lines, actions) = build_status_lines(7, &pr_info, Some(&review));

    // Should have a collapsed comment line
    assert!(lines.iter().any(|l| l.contains("[+] L42")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Please rename this variable"))
    );

    // Should have review body
    assert!(lines.iter().any(|l| l.contains("Review body:")));
    assert!(lines.iter().any(|l| l.contains("Looks good overall")));

    assert_eq!(lines.len(), actions.len());
}

#[test]
fn test_build_status_lines_viewed_files() {
    let pr_info = PrInfo {
        pr_number: 42,
        title: Some("Fix the widget".to_string()),
        base_branch: "main".to_string(),
        head_branch: Some("fix-widget".to_string()),
        files_changed: vec![
            FileChange {
                path: "src/lib.rs".to_string(),
                additions: 10,
                deletions: 3,
                status: "M".to_string(),
            },
            FileChange {
                path: "src/new.rs".to_string(),
                additions: 50,
                deletions: 0,
                status: "A".to_string(),
            },
        ],
    };

    let mut viewed = HashSet::new();
    viewed.insert("src/lib.rs".to_string());

    let review = Review {
        owner: "test".to_string(),
        repo: "repo".to_string(),
        backend: GitBackend::GitHub,
        backend_url: None,
        pr_number: 42,
        body: String::new(),
        comments: vec![],
        in_progress_comment: None,
        viewed_files: viewed,
    };

    let (lines, actions) = build_status_lines(42, &pr_info, Some(&review));

    // Header shows viewed count
    assert!(lines.iter().any(|l| l.contains("Files changed (1/2):")));

    // Viewed file has [x], unviewed has [ ]
    assert!(lines.iter().any(|l| l.contains("[x] M src/lib.rs")));
    assert!(lines.iter().any(|l| l.contains("[ ] A src/new.rs")));

    assert_eq!(lines.len(), actions.len());
}

#[test]
fn test_review_deserialization_backwards_compat() {
    let json = r#"{
        "owner": "test",
        "repo": "repo",
        "backend": "GitHub",
        "pr_number": 1,
        "body": "",
        "comments": [],
        "in_progress_comment": null
    }"#;
    let review: Review = serde_json::from_str(json).unwrap();
    assert!(review.viewed_files.is_empty());
}

#[test]
fn test_toggle_viewed() {
    let mut review = Review::new(
        "owner".to_string(),
        "repo".to_string(),
        GitBackend::GitHub,
        None,
        1,
        String::new(),
        vec![],
    );

    assert!(!review.is_viewed("src/lib.rs"));
    review.toggle_viewed("src/lib.rs");
    assert!(review.is_viewed("src/lib.rs"));
    review.toggle_viewed("src/lib.rs");
    assert!(!review.is_viewed("src/lib.rs"));
}

#[test]
fn test_file_change_serialization() {
    let fc = FileChange {
        path: "src/main.rs".to_string(),
        additions: 10,
        deletions: 5,
        status: "M".to_string(),
    };
    let json = serde_json::to_string(&fc).unwrap();
    let deserialized: FileChange = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.path, "src/main.rs");
    assert_eq!(deserialized.additions, 10);
    assert_eq!(deserialized.deletions, 5);
    assert_eq!(deserialized.status, "M");
}

#[test]
fn test_pr_info_serialization() {
    let pr_info = PrInfo {
        pr_number: 1,
        title: Some("Test PR".to_string()),
        base_branch: "main".to_string(),
        head_branch: Some("feature".to_string()),
        files_changed: vec![],
    };
    let json = serde_json::to_string(&pr_info).unwrap();
    let deserialized: PrInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.pr_number, 1);
    assert_eq!(deserialized.title, Some("Test PR".to_string()));
    assert_eq!(deserialized.base_branch, "main");
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

/// Set the provided text as the contents of the current buffer
fn set_text_in_buffer(text: String) -> ApiResult<()> {
    let mut buffer = api::get_current_buf();
    buffer.set_lines(0..10000000, false, text.split("\n"))?;
    Ok(())
}

/// Render `body` in a floating, read-only, LSP-style hover window anchored to the
/// cursor. The window dismisses itself on the next cursor move, leave, or insert-enter.
///
/// Routes the floating-window and option calls through VimL dispatch
/// (`nvim_call_function`) rather than nvim-oxi's typed `open_win` / `set_option_value`.
/// The `WindowOpts` FFI struct in nvim-oxi 0.6.0 (built with the `neovim-0-11`
/// feature) does not match nvim 0.12's ABI and aborts the process; routing through
/// VimL goes through nvim's own version-correct conversion layer.
fn show_comment_hover(body: &str) -> ApiResult<()> {
    let lines: Vec<&str> = body.split('\n').collect();
    let height = lines.len().clamp(1, 20) as i64;
    let width = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .clamp(20, 80) as i64;

    let mut buf = api::create_buf(false, true)
        .map_err(|_| api::Error::Other("Failed to create hover buffer".to_string()))?;
    buf.set_lines(.., false, lines.iter().copied())?;
    let buf_handle: i64 = {
        let obj: oxi::Object = (&buf).into();
        unsafe { obj.as_integer_unchecked() }
    };

    let buf_scope = Dictionary::from_iter([("buf", Object::from(buf_handle))]);
    let _: Object = api::call_function(
        "nvim_set_option_value",
        ("modifiable", false, buf_scope.clone()),
    )?;
    let _: Object = api::call_function(
        "nvim_set_option_value",
        ("filetype", "markdown", buf_scope),
    )?;

    let win_config = Dictionary::from_iter([
        ("relative", Object::from("cursor")),
        ("row", Object::from(1i64)),
        ("col", Object::from(0i64)),
        ("width", Object::from(width)),
        ("height", Object::from(height)),
        ("style", Object::from("minimal")),
        ("border", Object::from("rounded")),
        ("focusable", Object::from(false)),
    ]);
    let win_handle: i64 =
        api::call_function("nvim_open_win", (buf_handle, false, win_config))?;

    let win_scope = Dictionary::from_iter([("win", Object::from(win_handle))]);
    let _: Object =
        api::call_function("nvim_set_option_value", ("wrap", true, win_scope))?;

    api::command(&format!(
        "autocmd CursorMoved,CursorMovedI,BufLeave,InsertEnter <buffer> ++once lua pcall(vim.api.nvim_win_close, {}, true)",
        win_handle
    ))?;

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
    #[serde(default)]
    base_branch: Option<String>,
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileChange {
    path: String,
    additions: u32,
    deletions: u32,
    status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrInfo {
    pr_number: u32,
    title: Option<String>,
    base_branch: String,
    head_branch: Option<String>,
    files_changed: Vec<FileChange>,
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
    #[serde(default)]
    viewed_files: HashSet<String>,
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
            viewed_files: HashSet::new(),
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
        let base_url = self.backend_url.as_deref().unwrap_or("https://gitlab.com");

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
            let is_multi_line =
                comment.start_line.is_some() && comment.start_line.unwrap() != comment.line;

            let (line_start, line_end) = if is_multi_line {
                (comment.start_line.unwrap(), comment.line)
            } else {
                (comment.line, comment.line)
            };

            let new_line = if comment.side == Side::RIGHT {
                serde_json::Value::from(line_end)
            } else {
                serde_json::Value::Null
            };
            let old_line = if comment.side == Side::LEFT {
                serde_json::Value::from(line_end)
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
                (
                    serde_json::Value::from(comment_path.clone()),
                    serde_json::Value::Null,
                )
            } else {
                (
                    serde_json::Value::Null,
                    serde_json::Value::from(comment_path.clone()),
                )
            };

            // Build position object
            // For multi-line comments, use line_range instead of new_line/old_line
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
                // Compute SHA1 hash of the filepath
                // Line code format: <filepath_SHA>_<old>_<new>
                let mut hasher = Sha1::new();
                hasher.update(comment_path.as_bytes());
                let file_hash = format!("{:x}", hasher.finalize());

                // Get the repository to look up line mappings
                let repo = match Repository::open_from_env() {
                    Ok(r) => r,
                    Err(e) => {
                        api::err_writeln(&format!("Failed to open git repository: {}", e));
                        continue;
                    }
                };

                // Get old/new line mappings for start and end lines
                let (start_old, start_new) = match get_line_mapping(
                    &repo,
                    &comment_path,
                    base_sha,
                    head_sha,
                    line_start,
                    comment.side,
                ) {
                    Ok(mapping) => mapping,
                    Err(e) => {
                        api::err_writeln(&format!(
                            "Failed to get line mapping for {}: {}",
                            comment_path, e
                        ));
                        continue;
                    }
                };

                let (end_old, end_new) = match get_line_mapping(
                    &repo,
                    &comment_path,
                    base_sha,
                    head_sha,
                    line_end,
                    comment.side,
                ) {
                    Ok(mapping) => mapping,
                    Err(e) => {
                        api::err_writeln(&format!(
                            "Failed to get line mapping for {}: {}",
                            comment_path, e
                        ));
                        continue;
                    }
                };

                // Build line codes with both old and new line numbers
                let start_line_code = format!(
                    "{}_{}_{}",
                    file_hash,
                    start_old.map(|n| n.to_string()).unwrap_or("0".to_string()),
                    start_new.map(|n| n.to_string()).unwrap_or("0".to_string())
                );
                let end_line_code = format!(
                    "{}_{}_{}",
                    file_hash,
                    end_old.map(|n| n.to_string()).unwrap_or("0".to_string()),
                    end_new.map(|n| n.to_string()).unwrap_or("0".to_string())
                );

                let line_range = if comment.side == Side::RIGHT {
                    serde_json::json!({
                        "start": {
                            "line_code": start_line_code,
                            "type": "new",
                            "new_line": start_new,
                            "old_line": start_old,
                        },
                        "end": {
                            "line_code": end_line_code,
                            "type": "new",
                            "new_line": end_new,
                            "old_line": end_old,
                        }
                    })
                } else {
                    serde_json::json!({
                        "start": {
                            "line_code": start_line_code,
                            "type": "old",
                            "new_line": start_new,
                            "old_line": start_old,
                        },
                        "end": {
                            "line_code": end_line_code,
                            "type": "old",
                            "new_line": end_new,
                            "old_line": end_old,
                        }
                    })
                };
                position["line_range"] = line_range;
            }

            let discussion_payload = serde_json::json!({
                "body": comment.body,
                "position": position
            });

            api::out_write(string!(
                "Posting payload {:?} to GitLab\n",
                discussion_payload
            ));

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

    pub fn toggle_viewed(&mut self, path: &str) {
        if self.viewed_files.contains(path) {
            self.viewed_files.remove(path);
        } else {
            self.viewed_files.insert(path.to_string());
        }
    }

    pub fn is_viewed(&self, path: &str) -> bool {
        self.viewed_files.contains(path)
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
        file.write_all(serde_json::to_string(&self).unwrap().as_bytes())
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
                comment.path == path
                    && (comment.line == line
                        || (comment.start_line.is_some()
                            && comment.start_line.unwrap() <= line
                            && comment.line >= line))
            })
            .collect();
        if !eligible_comments.is_empty() {
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
                    None
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

/// Get the old and new line numbers for a given line in a diff
/// Returns (Option<old_line>, Option<new_line>)
/// If the line is only in the old file (deleted), new_line will be None
/// If the line is only in the new file (added), old_line will be None
fn get_line_mapping(
    repo: &Repository,
    _file_path: &str,
    base_sha: &str,
    head_sha: &str,
    line_number: u32,
    side: Side,
) -> Result<(Option<u32>, Option<u32>), String> {
    // Parse commit SHAs
    let base_oid = git2::Oid::from_str(base_sha).map_err(|e| format!("Invalid base SHA: {}", e))?;
    let head_oid = git2::Oid::from_str(head_sha).map_err(|e| format!("Invalid head SHA: {}", e))?;

    // Get commit objects
    let base_commit = repo
        .find_commit(base_oid)
        .map_err(|e| format!("Failed to find base commit: {}", e))?;
    let head_commit = repo
        .find_commit(head_oid)
        .map_err(|e| format!("Failed to find head commit: {}", e))?;

    // Get trees
    let base_tree = base_commit
        .tree()
        .map_err(|e| format!("Failed to get base tree: {}", e))?;
    let head_tree = head_commit
        .tree()
        .map_err(|e| format!("Failed to get head tree: {}", e))?;

    // Create diff
    let diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .map_err(|e| format!("Failed to create diff: {}", e))?;

    // Find the file in the diff and build line mapping
    let mut line_map: Vec<(Option<u32>, Option<u32>)> = Vec::new();

    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        None,
        Some(&mut |_delta, _hunk, line| {
            match line.origin() {
                ' ' => {
                    // Context line - exists in both old and new
                    line_map.push((line.old_lineno(), line.new_lineno()));
                }
                '-' => {
                    // Deleted line - only in old
                    line_map.push((line.old_lineno(), None));
                }
                '+' => {
                    // Added line - only in new
                    line_map.push((None, line.new_lineno()));
                }
                _ => {}
            }
            true
        }),
    )
    .map_err(|e| format!("Failed to process diff: {}", e))?;

    // Find the mapping for the requested line
    // The line_number is relative to the side (old or new)
    for (old_line, new_line) in &line_map {
        if side == Side::LEFT {
            // Looking for old line number
            if let Some(old) = old_line
                && *old == line_number
            {
                return Ok((*old_line, *new_line));
            }
        } else {
            // Looking for new line number
            if let Some(new) = new_line
                && *new == line_number
            {
                return Ok((*old_line, *new_line));
            }
        }
    }

    // If not found in diff, the line is unchanged - both old and new have same number
    Ok((Some(line_number), Some(line_number)))
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
    review_dir
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
    file.write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
}
