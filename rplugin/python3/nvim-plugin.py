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
        self.review = offline_pr_review.new_blank_review(args[0])
        self.review_active = True

    @pynvim.command('PublishReview')
    def publish_review(self):
        self.review_active = False
        self.review.publish(os.getenv("GH_API_TOKEN"))

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    @pynvim.command('ReviewComment', nargs='*', range='')
    def review_comment(self, args, range):
        if self.in_progress_comment is not None:
            self.nvim.err_write("A review comment is already being edited.")
            return

        with NamedTemporaryFile('w') as f:

            from remote_pdb import RemotePdb; RemotePdb('localhost', 56014).set_trace()
            # TODO: Determine the path in the git repo for a given buffer name
            # (self.nvim.current.buffer.name)
            self.in_progress_comment = offline_pr_review.Comment(
                body="",
                path=self.nvim.current.buffer.name,
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

            self.nvim.out_write("open trigger\n")
            # Set the on-save behavior for this buffer. This uses the buffer-local
            # autocommands feature.
            self.nvim.command(f'autocmd BufWritePre <buffer> :SaveComment {" ".join(range)}')


    @pynvim.command('SaveComment', sync=True, nargs='*')
    def save_comment(self, args):
        self.nvim.out_write("Save trigger\n")
        buffer_contents = self.nvim.current.buffer[:]
        self.nvim.out_write(f"{buffer_contents}\n")
        self.nvim.out_write(f"{args}\n")
        self.in_progress_comment.body = '\n'.join(buffer_contents)
        self.review.add_comment(self.in_progress_comment)
        self.in_progress_comment = None
