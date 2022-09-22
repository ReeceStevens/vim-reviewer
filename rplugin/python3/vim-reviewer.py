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
        self.nvim.command('sign define PrReviewComment text=C> texthl=Search linehl=DiffText')

    # TODO: Can only show signs for files that are already loaded in a buffer.
    # Need to update signs if a new file is opened.
    @pynvim.command("UpdateReviewSigns")
    def update_signs(self):
        if not self.is_review_active():
            return

        self.nvim.command('sign unplace * group=PrReviewSigns')
        self.sign_idx = 0
        for buffer in self.nvim.buffers:
            self.update_signs_in_buffer(buffer)

    def update_signs_in_buffer(self, buffer: pynvim.api.Buffer):
        comments_in_buffer = [
            c for c in self.review.comments
            if os.path.join(self.repository_absolute_path(), c.path) == buffer.name
        ]
        for comment in comments_in_buffer:
            start_line = comment.start_line or comment.line
            end_line = comment.line
            for line in range(start_line, end_line + 1):
                self.sign_idx += 1
                self.nvim.command(f'sign place {self.sign_idx} line={line} name=PrReviewComment group=PrReviewSigns buffer={buffer.handle}')


    def save(self):
        self.review.save()
        self.update_signs()

    @pynvim.command('StartReview', nargs=1)
    def start_review(self, args):
        self.review = offline_pr_review.get_or_create_review(args[0])
        self.review_active = True
        self.update_signs()

    @pynvim.command('PublishReview')
    def publish_review(self):
        """
        Publish the in-progress review to GitHub.
        """
        if self.review_active:
            self.review_active = False
            result = self.review.publish(os.getenv("GH_REVIEW_API_TOKEN"))
            self.nvim.out_write(f'{result}: {result.reason}\n')
            try:
                result.raise_for_status()
            except Exception as e:
                self.nvim.err_write(f'{result.text}\n')
            self.update_signs()
        else:
            self.nvim.err_write("Cannot publish since no review is currently active.\n")

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    def repository_absolute_path(self) -> str:
        return self.nvim.call('FugitiveWorkTree')

    def current_buffer_path(self) -> Optional[str]:
        """
        Return the buffer's current path in the git repository, or None if it does not exist.

        For example, a file called "test.py" within a parent directory called
        "project" would return the path `project/test.py`.
        """
        repository_root = self.nvim.call('FugitiveWorkTree')
        current_buffer_path = self.nvim.current.buffer.name
        if current_buffer_path.startswith('/') and repository_root:
            return current_buffer_path.replace(repository_root + '/', '')
        return None

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
        self.save()

    @pynvim.command('ReviewBody', sync=True)
    def review_body(self):
        if self.is_review_active():
            self.new_temporary_buffer(on_save_command='SaveReviewBody')
            self.nvim.current.buffer[:] = self.review.body.split('\n')
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
            self.save()

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

    @pynvim.command('DeleteComment', nargs="*", range="")
    def delete_comment(self, args, range):
        """
        Delete the comment for the line under the cursor, if one exists.
        """
        path = self.current_buffer_path()
        if path is None:
            self.nvim.err_write("Current buffer is not a valid path in the git repository.\n")
            return
        comment_to_delete = self.review.get_comment_at_position(path, range[0])
        if comment_to_delete is None:
            self.nvim.err_write("No comment under the cursor.\n")
            return

        self.review.delete_comment(comment_to_delete)
        self.nvim.out_write("Comment deleted.\n")
        self.update_signs()
