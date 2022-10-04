extern crate git2;
extern crate nvim_oxi;
extern crate reqwest;
extern crate serde;
extern crate tempfile;

use git2::Repository;
use nvim_oxi::api;
use nvim_oxi::opts::CreateCommandOpts;
use nvim_oxi::types::{CommandArgs, CommandNArgs, CommandRange};
use nvim_oxi::{self as oxi, Dictionary, Array};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
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
        api::create_user_command($name, $fn, Some(&opts))?;
    };
}


#[oxi::module]
fn vim_reviewer() -> oxi::Result<()> {
    // TODO: work out better handling for this `review` global.
    static mut REVIEW: Box<Option<Review>> = Box::new(None);
    static mut IN_PROGRESS_COMMENT: Option<Comment> = None;

    // let repo = match Repository::open(env::current_dir().unwrap()) {
    //     Ok(repo) => repo,
    //     Err(e) => {
    //         api::err_writeln(&format!("Current directory is not a git repository: {}", e));
    //         unimplemented!() // TODO: Determine better errror return
    //     }
    // };

    // let remote_info = api::call_function("FugitiveRemote", vec![]).unwrap();
    // TODO: Update configuration based on remote settings
    api::command("sign define PrReviewComment text=C> texthl=Search linehl=DiffText")?;

    create_command!(
        "UpdateReviewSigns",
        "Update the gutter symbols for review comments",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                match &*REVIEW {
                    None => return Ok(()),
                    Some(review) => {
                        let mut sign_idx = 0;
                        api::command("sign unplace * group=PrReviewSigns")?;
                        for buffer in api::list_bufs() {
                            let buffer_path = get_current_buffer_path()?;
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
                                for line in start_line..=end_line {
                                    sign_idx += 1;
                                    api::command(&format!(
                                    "sign place {} line={} name=PrReviewComment group=PrReviewSigns file={}",
                                    sign_idx,
                                    line,
                                    buffer.get_name().unwrap().display()
                                ))
                                .unwrap();
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    );

    create_command!(
        "StartReview",
        "Start a review",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                REVIEW = Box::new(Some(Review::get_review(
                    str::parse::<u32>(&args.args.unwrap()).unwrap(),
                )));
            }
            // update_signs()?;
            Ok(())
        }
    );

    create_command!(
        "PublishReview",
        "Publish a review to GitHub",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                match &*REVIEW {
                    Some(review) => {
                        review
                            .publish(env::var("GH_REVIEW_API_TOKEN").unwrap())
                            .unwrap();
                        // update_signs();
                    }
                    None => {
                        api::err_writeln("Cannot publish since no review is currently active.");
                    }
                };
                Ok(())
            }
        }
    );

    create_command!(
        "ReviewComment",
        "Add a review comment",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            // if in_progress_comment.is_some() {
            //     api::err_writeln("A review comment is already being edited.");
            //     return Ok(());
            // }

            let path = get_current_buffer_path()?;
            let multi_line = args.line1 != args.line2;
            unsafe {
                IN_PROGRESS_COMMENT = Some(Comment::new(
                    "".to_string(),
                    args.line2 as u32,
                    path.to_str().unwrap().to_string(),
                    Side::RIGHT,
                    Some(if multi_line {
                        args.line1 as u32
                    } else {
                        (args.line1 - 1) as u32
                    }),
                    Some(Side::RIGHT),
                ));
            }

            new_temporary_buffer(Some("SaveComment new"))?;

            Ok(())
        }
    );

    create_command!(
        "SaveComment",
        "Save an in-progress review comment",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            let is_new_comment = args.args.unwrap_or("".to_string()) == "new".to_string();
            unsafe {
                match &mut IN_PROGRESS_COMMENT {
                    Some(comment) => {
                        comment.body = get_text_from_current_buffer()?;
                        IN_PROGRESS_COMMENT = None;
                        match &*REVIEW {
                            Some(review) => {
                                if is_new_comment {
                                    review.add_comment(comment.clone());
                                }
                                review.save();
                            }
                            None => {
                                api::err_writeln("No in-progress review");
                            }
                        };
                    }
                    None => {
                        api::err_writeln("No in-progress comment to save.");
                    }
                };
            }
            Ok(())
        }
    );

    create_command!(
        "ReviewBody",
        "Edit the body text of the review",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                match &*REVIEW {
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
        }
    );

    create_command!(
        "SaveReviewBody",
        "Save the buffer contents to the review body",
        CommandNArgs::ZeroOrOne,
        |_args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                match &*REVIEW {
                    None => {
                        api::err_writeln("No review is currently active.");
                        Ok(())
                    }
                    Some(review) => {
                        review.body = get_text_from_current_buffer()?;
                        review.save();
                        Ok(())
                    }
                }
            }
        }
    );

    create_command!(
        "EditComment",
        "Save the buffer contents to the review body",
        CommandNArgs::ZeroOrOne,
        |args: CommandArgs| -> oxi::Result<()> {
            let path = get_current_buffer_path()?;
            unsafe {
                match &*REVIEW {
                    None => {
                        api::err_writeln("No review is currently active.");
                        Ok(())
                    }
                    Some(review) => {
                        let comment_to_edit = review.get_comment_at_position(
                            path.to_str().unwrap().to_string(),
                            args.line1 as u32,
                        );
                        match comment_to_edit {
                            None => {
                                api::err_writeln("No comment under the cursor.");
                                Ok(())
                            },
                            Some(comment) => {
                                // TODO: in progress comment management
                                new_temporary_buffer(Some("SaveComment existing"))?;
                                set_text_in_buffer(comment.body.clone())?;
                                Ok(())
                            },
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
        |args: CommandArgs| -> oxi::Result<()> {
            let path = get_current_buffer_path()?;
            unsafe {
                match &*REVIEW {
                    None => {
                        api::err_writeln("No review is currently active.");
                        Ok(())
                    }
                    Some(review) => {
                        let comment_to_delete = review.get_comment_at_position(
                            path.to_str().unwrap().to_string(),
                            args.line1 as u32,
                        );
                        match comment_to_delete {
                            None => {
                                api::err_writeln("No comment under the cursor.");
                                Ok(())
                            },
                            Some(comment) => {
                                // TODO: Messy handling of comment deletion
                                review.delete_comment(&comment.clone());
                                api::out_write("Comment deleted.\n");
                                Ok(())
                            }
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
        |args: CommandArgs| -> oxi::Result<()> {
            unsafe {
                match &*REVIEW {
                    None => {
                        api::err_writeln("No review is currently active.");
                        Ok(())
                    },
                    Some(review) => {
                        let comments: Array = review.comments.iter().map(|comment| {
                            Dictionary::from_iter([
                                ("filename", comment.path.clone()),
                                ("lnum", comment.line.to_string()),
                                ("text", comment.body.clone()),
                            ])
                        }).collect();
                        api::call_function::<Array, ()>("setqflist", comments)?;
                        Ok(())

                    },
                }
            }
        }
    );


    Ok(())
}

/// Open a new temporary buffer. If `on_save_command` is specified, run the command on BufWritePre
/// on the new buffer.
fn new_temporary_buffer(on_save_command: Option<&str>) -> nvim_oxi::Result<()> {
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
fn get_text_from_current_buffer() -> nvim_oxi::Result<String> {
    Ok(api::get_current_buf()
        .get_lines(0, 10000000, false)?
        .map(|s| s.to_string())
        .collect::<Vec<String>>()
        .join("\n"))
}

/// Get the relative path in the repository for the file open in the current buffer.
fn get_current_buffer_path() -> nvim_oxi::Result<PathBuf> {
    let repo = Repository::open_from_env().unwrap();
    let workdir = repo.workdir().unwrap();
    let current_buffer = api::get_current_buf();
    let buffer_path = current_buffer.get_name().unwrap();

    match buffer_path.strip_prefix(workdir) {
        Err(e) => {
            api::err_writeln(&format!(
                "Current buffer is not a valid path in the git repository: {}",
                e
            ));
            Err(nvim_oxi::Error::Other(
                "Current buffer not a valid path in the repository".to_string(),
            ))
        }
        Ok(path) => Ok(path.to_path_buf()),
    }
}

/// Set the provided text as the contents of the current buffer
fn set_text_in_buffer(text: String) -> nvim_oxi::Result<()> {
    let mut buffer = api::get_current_buf();
    buffer.set_lines(0, 10000000, false, text.split("\n"))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    owner: String,
    repo: String,
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy)]
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
    pub fn get_comment_at_position(&self, path: String, line: u32) -> Option<&Comment> {
        let eligible_comments: Vec<&Comment> = self
            .comments
            .iter()
            .filter(|comment| {
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
        Err(e) => panic!("Could not open configuration file {}: {}", config_file_path.display(), e),
        Ok(mut file) => {
            file.read_to_string(&mut config_string).unwrap();
        }
    }
    serde_json::from_str(&config_string).unwrap()
}

pub fn update_configuration(config: Config) {
    let config_file_path = get_config_file_path();
    if config_file_path.exists() {
        // TODO: find better way to manage this warning. Or is it even necessary?
        println!("Warning: overwriting existing configuration.");
    }
    let mut file = match File::create(&config_file_path) {
        Err(err) => panic!("Error creating {}: {}", config_file_path.display(), err),
        Ok(file) => file,
    };
    file.write_all(&serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
}
