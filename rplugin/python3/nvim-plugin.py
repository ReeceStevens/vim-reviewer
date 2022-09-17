import os
from typing import Optional
from tempfile import NamedTemporaryFile

import pynvim
import offline_pr_review


@pynvim.plugin
class TestPlugin(object):
    review_active: bool
    review: Optional[offline_pr_review.Review]
    in_progress_comment: Optional[offline_pr_review.Comment]

    def __init__(self, nvim):
        self.review_active = False
        self.nvim = nvim
        self.in_progress_comment = None
        remote_info = self.nvim.call('FugitiveRemote')
        offline_pr_review.update_configuration(remote_info['path'].replace('.git', ''))

    @pynvim.command('StartReview', nargs=1)
    def start_review(self, args):
        # TODO: If review already exists for this PR, load it from disk
        self.review = offline_pr_review.new_blank_review(args[0])
        self.review_active = True

    @pynvim.command('PublishReview', sync=True)
    def publish_review(self):
        self.review_active = False
        result = self.review.publish(os.getenv("GH_API_TOKEN"))
        self.nvim.out_write(f'{result}\n')

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    @pynvim.command('ReviewComment', sync=True, nargs='*', range='')
    def review_comment(self, args, range):
        if self.in_progress_comment is not None:
            self.nvim.err_write("A review comment is already being edited.\n")
            return

        with NamedTemporaryFile('w') as f:
            git_dir_path = self.nvim.call('FugitiveGitDir')
            current_buffer_path = self.nvim.current.buffer.name
            if current_buffer_path.startswith('/') and git_dir_path:
                repository_root = git_dir_path[:-len('.git')]
                path = current_buffer_path.replace(repository_root, '')
            else:
                # TODO: error handling for trying to comment on a buffer that isn't a file in the repo
                return
            self.in_progress_comment = offline_pr_review.Comment(
                body="",
                path=path,
                line=range[1],
                start_line=range[0],
                # TODO: eventually get better side detection from buffer names
                side='RIGHT',
                start_side='RIGHT'
            )
            # Open a new buffer and focus it
            self.nvim.command(f'sp {f.name}')
            # Use markdown highlighting
            self.nvim.command('set ft=markdown')

            # Set the on-save behavior for this buffer. This uses the buffer-local
            # autocommands feature.
            self.nvim.command('autocmd BufWritePre <buffer> :SaveComment')


    @pynvim.command('SaveComment', sync=True)
    def save_comment(self):
        buffer_contents = self.nvim.current.buffer[:]
        self.in_progress_comment.body = '\n'.join(buffer_contents)
        self.review.add_comment(self.in_progress_comment)
        self.in_progress_comment = None
        self.review.save()
