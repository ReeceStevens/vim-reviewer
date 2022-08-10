from argparse import ArgumentParser
from dataclasses import dataclass
import os
import json
from typing import List, Optional, Literal

import requests
import github


@dataclass
class Comment:
    body: str
    line: int
    path: str
    side: Literal["RIGHT", "LEFT"]
    start_line: Optional[int]
    start_side: Optional[Literal["RIGHT", "LEFT"]]

    def serialize(self) -> str:
        return json.dumps(
            {
                "body": self.body,
                "path": self.path,
                "line": self.line,
                "side": self.side,
                "start_line": self.start_line,
                "start_side": self.start_side,
            }
        )

    @staticmethod
    def deserialize(serialized: str) -> "Comment":
        dict_representation = json.loads(serialized)
        return Comment(
            dict_representation["body"],
            dict_representation["line"],
            dict_representation["path"],
            dict_representation["side"],
            dict_representation["start_line"],
            dict_representation["start_side"],
        )


@dataclass
class Review:
    owner: str
    repo: str
    pr_number: int
    # TODO: Better token handling
    token: str
    body: str
    comments: List[Comment]

    def serialize(self) -> str:
        return json.dumps(
            {
                "body": self.body,
                "comments": [comment.serialize() for comment in self.comments],
            }
        )

    @property
    def post_url(self):
        return f"https://api.github.com/repos/{self.owner}/{self.repo}/pulls/{self.pr_number}/reviews"

    def publish(self):
        return requests.post(
            self.post_url,
            data=self.serialize(),
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"token {self.token}",
            },
        )

    @staticmethod
    def deserialize(serialized: str) -> "Review":
        dict_representation = json.loads(serialized)
        return Review(
            dict_representation["owner"],
            dict_representation["repo"],
            dict_representation["pr_number"],
            dict_representation["token"],
            dict_representation["body"],
            [Comment.deserialize(c) for c in dict_representation["comments"]],
        )


def get_args():
    parser = ArgumentParser()
    subparsers = parser.add_subparsers(help="sub-command help")
    init_parser = subparsers.add_parser("init", help="Initialize review directory")
    init_parser.add_argument("--init-dir", default=None)
    start_parser = subparsers.add_parser("start")
    return parser.parse_args()


def main(args):
    pass


if __name__ == "__main__":
    args = get_args()
