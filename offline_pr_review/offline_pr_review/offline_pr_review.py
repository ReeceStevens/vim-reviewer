from argparse import ArgumentParser
from dataclasses import dataclass
import os
import json
from typing import List, Optional, Literal, Tuple
import subprocess

import requests

Side = Literal["RIGHT", "LEFT"]


@dataclass
class Comment:
    body: str
    line: int
    path: str
    side: Side
    start_line: Optional[int]
    start_side: Optional[Side]

    def to_json(self) -> dict:
        return {
            "body": self.body,
            "path": self.path,
            "line": self.line,
            "side": self.side,
            "start_line": self.start_line,
            "start_side": self.start_side,
        }

    def serialize(self) -> str:
        return json.dumps(self.to_json(), indent=2)

    @staticmethod
    def from_json(json_repr: dict) -> "Comment":
        return Comment(
            json_repr["body"],
            json_repr["line"],
            json_repr["path"],
            json_repr["side"],
            json_repr["start_line"],
            json_repr["start_side"],
        )

    @staticmethod
    def deserialize(serialized: str) -> "Comment":
        return Comment.from_json(json.loads(serialized))


@dataclass
class Review:
    owner: str
    repo: str
    pr_number: int
    body: str
    comments: List[Comment]

    def to_json(self) -> dict:
        return {
            "owner": self.owner,
            "repo": self.repo,
            "pr_number": self.pr_number,
            "body": self.body,
            "comments": [comment.to_json() for comment in self.comments],
        }

    def serialize(self) -> str:
        return json.dumps(self.to_json(), indent=2)

    @property
    def post_url(self):
        return f"https://api.github.com/repos/{self.owner}/{self.repo}/pulls/{self.pr_number}/reviews"

    def publish(self, token):
        return requests.post(
            self.post_url,
            data=self.serialize(),
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"token {token}",
            },
        )

    def add_comment(self, comment: Comment):
        self.comments.append(comment)

    def set_body(self, body: str):
        self.body = body

    def save(self):
        review_file = get_review_file(self.pr_number)
        with open(review_file, "w") as f:
            f.write(self.serialize())

    @staticmethod
    def from_json(json_repr: dict) -> "Review":
        return Review(
            json_repr["owner"],
            json_repr["repo"],
            json_repr["pr_number"],
            json_repr["body"],
            [Comment.from_json(c) for c in json_repr["comments"]],
        )

    @staticmethod
    def deserialize(serialized: str) -> "Review":
        return Review.from_json(json.loads(serialized))

    def get_comment_at_position(self, path: str, line: int) -> Optional[Comment]:
        """
        Return the first comment in this review whose span contains the
        requested file path and line.
        """
        eligible_comments = [
            c for c in self.comments
            if c.path == path and (
                line == c.line or (c.start_line is not None and (line >= c.start_line) and (line <= c.line))
            )
        ]
        if eligible_comments:
            return eligible_comments[0]
        return None


def get_review_directory() -> str:
    """
    Returns the directory storing in-progress reviews. Creates this directory if it does not exist.

    This directory is within the `.git` directory of the local repository.
    """
    git_dir = (
        subprocess.check_output(["git", "rev-parse", "--git-dir"])
        .decode("utf-8")
        .strip()
    )
    reviews_path = os.path.join(git_dir, "reviews")
    os.makedirs(reviews_path, exist_ok=True)
    print(f"Review directory at {reviews_path}")
    return reviews_path


def get_review_file(pr_number: int) -> str:
    """
    Return the path to the review file for the PR specified by `pr_number`.
    """
    review_directory = get_review_directory()
    return os.path.join(review_directory, f"{pr_number}-review.json")

def get_or_create_review(pr_number: int) -> Review:
    review_file = get_review_file(pr_number)
    if os.path.exists(review_file):
        with open(review_file) as f:
            return Review.deserialize(f.read())
    else:
        return new_blank_review(pr_number)


def get_repo_from_config() -> Tuple[str, str]:
    config_path = get_config_file_path()
    with open(config_path) as f:
        config = json.load(f)
        return config["owner"], config["repo"]


def new_blank_review(pr_number: int) -> Review:
    owner, repo = get_repo_from_config()
    return Review(owner, repo, pr_number, "", [])


def get_review(pr_number: int) -> Review:
    """
    Return the `Review` object representing the current review for `pr_number`.
    Creates a new `Review` object if no review already exists.
    """
    review_file_path = get_review_file(pr_number)
    if os.path.exists(review_file_path):
        with open(review_file_path) as f:
            return Review.deserialize(f.read())
    else:
        return new_blank_review(pr_number)


def get_config_file_path():
    review_dir = get_review_directory()
    return os.path.join(review_dir, "config.json")


def update_configuration(repository: str):
    config_file_path = get_config_file_path()
    if os.path.exists(config_file_path):
        print("Warning: overwriting existing configuration.")
    owner, repo = repository.split("/")
    with open(config_file_path, "w") as f:
        json.dump({"owner": owner, "repo": repo}, f, indent=2)


def add_comment(
    pull_request: int,
    body: str,
    line: int,
    path: str,
    side: Side,
    start_line: Optional[int] = None,
    start_side: Optional[Side] = None,
):
    review = get_review(pull_request)
    review.add_comment(
        Comment(
            body,
            line,
            path,
            side,
            start_line,
            start_side,
        )
    )
    review.save()


def list_comments(pull_request: int):
    review = get_review(pull_request)
    print(json.dumps([c.to_json() for c in review.comments], indent=2))


def add_review(pull_request: int, body: str):
    review = get_review(pull_request)
    review.set_body(body)
    review.save()


def list_review(pull_request: int):
    review = get_review(pull_request)
    print(review.body)


def submit_review(pull_request: int):
    review = get_review(pull_request)
    review.publish(os.getenv("GH_API_TOKEN"))


def get_args():
    parser = ArgumentParser()
    subparsers = parser.add_subparsers(help="sub-command help")

    config_parser = subparsers.add_parser("config")
    config_parser.add_argument(
        "repository", help="Repository name in the format of {owner}/{repo}"
    )
    config_parser.set_defaults(
        func=(lambda args: update_configuration(args.repository(args)))
    )

    comment_parser = subparsers.add_parser("comment")
    comment_subparser = comment_parser.add_subparsers()
    add_comment_parser = comment_subparser.add_parser(
        "add", help="Add a new comment to a pull request review"
    )
    add_comment_parser.add_argument(
        "--pull-request", help="Pull request number to review"
    )
    add_comment_parser.add_argument("--body", help="body text of the comment to add")
    add_comment_parser.add_argument("--line", help="body text of the comment to add")
    add_comment_parser.add_argument("--path", help="body text of the comment to add")
    add_comment_parser.add_argument(
        "--side",
        help="body text of the comment to add",
        required=False,
        default="RIGHT",
    )
    add_comment_parser.add_argument(
        "--start-line", help="body text of the comment to add", required=False
    )
    add_comment_parser.add_argument(
        "--start-side", help="body text of the comment to add", required=False
    )
    add_comment_parser.set_defaults(
        func=(
            lambda args: add_comment(
                args.pull_request,
                args.body,
                args.line,
                args.path,
                args.side,
                args.start_line,
                args.start_side,
            )
        )
    )
    list_comments_parser = comment_subparser.add_parser(
        "list", help="List the comments for a given pull request review"
    )
    list_comments_parser.add_argument(
        "--pull-request", help="Pull request number for which to view comments"
    )
    list_comments_parser.set_defaults(func=(lambda args: list_comments(args.pull_request)))

    review_parser = subparsers.add_parser("review")
    review_subparser = review_parser.add_subparsers()
    add_review_parser = review_subparser.add_parser(
        "add", help="Add a top-level review comment to a PR review"
    )
    add_review_parser.add_argument(
        "--pull-request", help="Pull request number to review"
    )
    add_review_parser.add_argument("--body", help="body text of the review to add")
    add_review_parser.set_defaults(func=(lambda args: add_review(args.pull_request, args.body)))
    list_review_parser = review_subparser.add_parser(
        "list", help="List any existing top-level review comment for a given PR review"
    )
    list_review_parser.add_argument(
        "--pull-request",
        help="Pull request number for which to view the top-level review comment",
    )
    list_review_parser.set_defaults(func=(lambda args: list_review(args.pull_request)))

    submit_parser = subparsers.add_parser("submit")
    submit_parser.add_argument(
        "--pull-request", help="Pull request number for which to submit a review"
    )
    submit_parser.set_defaults(func=(lambda args: submit_review(args.pull_request)))

    return parser.parse_args()


if __name__ == "__main__":
    args = get_args()
    args.func(args)
