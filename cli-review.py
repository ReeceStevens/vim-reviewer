from argparse import ArgumentParser
import os

import github


def get_args():
    parser = ArgumentParser()
    parser.add_argument("--start-review")
    parser.add_argument("branch")
    return parser.parse_args()


def main(args):
    gh = github.Github(os.environ.get('GH_API_TOKEN'))
    repo = gh.get_repo('reecestevens/cli-github-pr-review')


if __name__ == "__main__":
    args = get_args()
    main(args)
