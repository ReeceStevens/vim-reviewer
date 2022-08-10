from argparse import ArgumentParser

import github

def get_args():
    parser = ArgumentParser()
    parser.add_argument('--start-review')
    parser.add_argument('branch')
    return parser.parse_args()

def main(args):
    pass

if __name__ == '__main__':
    args = get_args()
    main(args)
