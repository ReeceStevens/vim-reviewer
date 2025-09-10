extern crate git2;
extern crate nvim_oxi;
extern crate reqwest;
extern crate serde;
extern crate tempfile;

use git2::Repository;
use nvim_oxi::api;
use nvim_oxi::string;
use nvim_oxi::api::opts::{CmdOpts, CreateCommandOpts};
use nvim_oxi::api::types::{CmdInfos, CommandArgs, CommandNArgs, CommandRange};
use nvim_oxi::{self as oxi, Array, Dictionary, Object};
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

/// Based on the remote URL, parse out the repository name and owner.
///
/// TODO: Currently, this assumes an SSH-formatted remote URL, such as
/// git@github.com:reecestevens/vim-reviewer.
///
/// Future work: this function can be used to determine which git backend is used (GitHub, GitLab,
/// etc)
fn parse_config_from_url(url: &str) -> Result<(String, String), String> {
    let repository_info = url.split(":").last();
    let results = match repository_info {
        Some(info) => info.split("/").collect::<Vec<&str>>(),
        None => return Err("Invalid repository url".to_string()),
    };
    Ok((
        results[0].to_string(),
        results[1].to_string().replace(".git", ""),
    ))
}

/// Update the repository configuration based on the current origin remote
fn update_config_from_remote() -> oxi::Result<()> {
    let repo = match Repository::open(env::current_dir().unwrap()) {
        Ok(repo) => repo,
        Err(e) => {
            api::err_writeln(&format!("Current directory is not a git repository: {}", e));
            return Ok(());
        }
    };
    let remote_url = match repo.find_remote("origin") {
        Ok(remote) => remote.url().unwrap().to_string(),
        Err(e) => {
            api::err_writeln(&format!("Failed to find remote 'origin': {}", e));
            return Ok(());
        }
    };
    let (owner, repo) = match parse_config_from_url(&remote_url) {
        Ok(results) => results,
        Err(_) => {
            api::err_writeln("Failed to parse repository information from remote URL");
            return Ok(());
        }
    };

    update_configuration(Config {
        owner,
        repo,
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
                                api::out_write(
                                    string!("{:?}: {}-{}\n", buffer, start_line, end_line),
                                );
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
            let mut config = get_config_from_file();
            config.active_pr = Some(str::parse::<u32>(&args.args.unwrap()).unwrap());
            update_configuration(config);
            Ok(())
        }
    );

    create_command!(
        "PublishReview",
        "Publish a review to GitHub",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> ApiResult<()> {
            let review = get_current_review();
            match review {
                Some(review) => {
                    let token = match env::var("GH_REVIEW_API_TOKEN") {
                        Ok(token) => token,
                        Err(e) => {
                            api::err_writeln(&format!(
                                "GH_REVIEW_API_TOKEN environment variable not set: {}",
                                e
                            ));
                            return Ok(());
                        }
                    };
                    match review.publish(token) {
                        Ok(response) => {
                            let status = response.status();
                            if status.is_success() {
                                api::out_write("Review published successfully\n");
                            } else {
                                api::err_writeln(&format!(
                                    "Failed to publish review ({:?}): {:?}",
                                    status,
                                    response.text()
                                ));
                            }
                        }
                        Err(error) => {
                            api::err_writeln(&format!(
                                "Failed to publish review due to error: {}",
                                error
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
    match config.active_pr {
        None => None,
        Some(pr_number) => Some(Review::get_review(pr_number)),
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
        Ok(path) => Ok((if buffer_is_prior_rev { Side::LEFT } else { Side::RIGHT }, path.to_path_buf())),
    }
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
    api::feedkeys(string!("Test.").as_nvim_str(), string!("i").as_nvim_str(), false);
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
    pr_number: u32,
    body: String,
    comments: Vec<Comment>,
    in_progress_comment: Option<Comment>,
}

impl Review {
    fn new(
        owner: String,
        repo: String,
        pr_number: u32,
        body: String,
        comments: Vec<Comment>,
    ) -> Self {
        Review {
            owner,
            repo,
            pr_number,
            body,
            comments,
            in_progress_comment: None,
        }
    }

    fn post_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
            self.owner, self.repo, self.pr_number
        )
        .to_string()
    }

    pub fn publish(&self, token: String) -> Result<reqwest::blocking::Response, reqwest::Error> {
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

    pub fn add_comment(&mut self, comment: Comment) {
        self.comments.push(comment);
    }

    pub fn set_body(&mut self, body: String) {
        self.body = body;
    }

    pub fn save(&self) {
        let review_file_path = get_review_file_path(self.pr_number);
        let mut file = match File::create(&review_file_path) {
            Err(err) => panic!("Error creating {}: {}", review_file_path.display(), err),
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

    pub fn get_review(pr_number: u32) -> Self {
        let review_file_path = get_review_file_path(pr_number);
        if review_file_path.exists() {
            let mut review_string = String::new();
            match File::open(review_file_path) {
                Err(e) => panic!("Could not open review file: {}", e),
                Ok(mut file) => {
                    file.read_to_string(&mut review_string).unwrap();
                }
            }
            serde_json::from_str(&review_string).unwrap()
        } else {
            // New review
            let config = get_config_from_file();
            Review::new(
                config.owner.to_string(),
                config.repo.to_string(),
                pr_number,
                "".to_string(),
                vec![],
            )
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

fn get_config_from_file() -> Config {
    let config_file_path = get_config_file_path();
    let mut config_string = String::new();
    match File::open(&config_file_path) {
        Err(e) => panic!(
            "Could not open configuration file {}: {}",
            config_file_path.display(),
            e
        ),
        Ok(mut file) => {
            file.read_to_string(&mut config_string).unwrap();
        }
    }
    serde_json::from_str(&config_string).unwrap()
}

pub fn update_configuration(config: Config) {
    let config_file_path = get_config_file_path();
    let mut file = match File::create(&config_file_path) {
        Err(err) => panic!("Error creating {}: {}", config_file_path.display(), err),
        Ok(file) => file,
    };
    file.write_all(&serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
}
