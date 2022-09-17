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

    def __init__(self, nvim: pynvim.api.Nvim):
        self.review_active = False
        self.nvim = nvim
        self.in_progress_comment = None
        # Determine the upstream github URL from the configured git remote
        remote_info = self.nvim.call('FugitiveRemote')
        offline_pr_review.update_configuration(remote_info['path'].replace('.git', ''))

    @pynvim.command('StartReview', nargs=1)
    def start_review(self, args):
        # TODO: If review already exists for this PR, load it from disk
        self.review = offline_pr_review.new_blank_review(args[0])
        self.review_active = True

    @pynvim.command('PublishReview')
    def publish_review(self):
        """
        Publish the in-progress review to GitHub.
        """
        if self.review_active:
            self.review_active = False
            result = self.review.publish(os.getenv("GH_API_TOKEN"))
            self.nvim.out_write(f'{result}\n')
        else:
            self.nvim.err_write("Cannot publish since no review is currently active.\n")

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    def current_buffer_path(self) -> Optional[str]:
        """
        Return the buffer's current path in the git repository, or None if it does not exist.

        For example, a file called "test.py" within a parent directory called
        "project" would return the path `project/test.py`.
        """
        git_dir_path = self.nvim.call('FugitiveGitDir')
        current_buffer_path = self.nvim.current.buffer.name
        if current_buffer_path.startswith('/') and git_dir_path:
            repository_root = git_dir_path[:-len('.git')]
            return current_buffer_path.replace(repository_root, '')
        return None

    # TODO: View existing comments
    # TODO: Delete an existing comment
    # TODO: Add additional comments to an already-published review

    def new_temporary_buffer(self, on_save_command: Optional[str] = None):
        """
        Create a new buffer for a temporary file and open it in a split.

        Additionally, set the provided `on_save_command` to be executed on
        buffer save.
        """
        with NamedTemporaryFile('w') as f:
            # Open a new buffer and focus it
            self.nvim.command(f'sp {f.name}')
            # Use markdown highlighting
            self.nvim.command('set ft=markdown')
            if on_save_command:
                # Set the on-save behavior for this buffer. This uses the buffer-local
                # autocommands feature.
                self.nvim.command(f'autocmd BufWritePre <buffer> :{on_save_command}')

    def current_buffer_contents(self) -> str:
        buffer_contents = self.nvim.current.buffer[:]
        return '\n'.join(buffer_contents)

    @pynvim.command('ReviewComment', sync=True, nargs="?", range='')
    def review_comment(self, args, range):
        """
        Initiate a review comment for the given range selection.

        This will open up a new buffer for the comment. The comment is saved to
        disk at every write.
        """
        if self.in_progress_comment is not None:
            self.nvim.err_write("A review comment is already being edited.\n")
            return

        path = self.current_buffer_path()
        if path is None:
            self.nvim.err_write("Current buffer is not a valid path in the git repository.\n")
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
        self.new_temporary_buffer(on_save_command='SaveComment new')


    @pynvim.command('SaveComment', nargs="*", sync=True)
    def save_comment(self, args):
        """
        Save the contents of the comment buffer to disk.

        This command is set to be triggered on `BufWritePre` for the comment
        buffer (e.g., on every write).

        Note that this command _must_ be `sync=True`, otherwise the buffer
        contents will be empty before they can be accessed in the case of a
        save-and-exit command (`:wq`).
        """
        is_new_comment = args[0] == 'new'
        self.in_progress_comment.body = self.current_buffer_contents()
        if is_new_comment:
            self.review.add_comment(self.in_progress_comment)
        self.in_progress_comment = None
        self.review.save()

    @pynvim.command('ReviewBody', sync=True)
    def review_body(self):
        if self.is_review_active():
            self.new_temporary_buffer(on_save_command='SaveReviewBody')
            self.nvim.current.buffer[:] = self.review.body
        else:
            self.nvim.err_write("No review is currently active.\n")

    @pynvim.command('SaveReviewBody', sync=True)
    def save_review_body(self):
        """
        Save the contents of the review body buffer to disk.

        This command is set to be triggered on `BufWritePre` for the review body
        buffer (e.g., on every write).

        Note that this command _must_ be `sync=True`, otherwise the buffer
        contents will be empty before they can be accessed in the case of a
        save-and-exit command (`:wq`).
        """
        if self.is_review_active():
            self.review.body = self.current_buffer_contents()
            self.review.save()

    @pynvim.command('EditComment', nargs="*", range="")
    def edit_comment(self, args, range):
        """
        Open up the comment for the line under the cursor, if one exists.
        """
        path = self.current_buffer_path()
        if path is None:
            self.nvim.err_write("Current buffer is not a valid path in the git repository.\n")
            return
        comment_to_edit = self.review.get_comment_at_position(path, range[0])
        if comment_to_edit is None:
            self.nvim.err_write("No comment under the cursor.\n")
            return

        self.in_progress_comment = comment_to_edit
        self.new_temporary_buffer(on_save_command='SaveComment existing')
        self.nvim.current.buffer[:] = self.in_progress_comment.body.split('\n')
