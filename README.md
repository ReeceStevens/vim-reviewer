# Testing

In a neovim installation with `vim-fugitive` installed, source the `.nvimrc`
local to this repository. Run `:UpdateRemotePlugins`, then re-open vim. Ensure
that there is a local virtualenv `./cli-review-venv`, which has an editable
install of `offline_pr_review` and `requests`.

# Use

Open a file in a git repository and run `:StartReview <pr-number>`-- for
example, `:StartReview 1`.

After that, navigate to the files you want to review. Leave a comment on a
single line or a range by using `:ReviewComment`.

Type your comment into the buffer, then save and exit.

Once you're done leaving comments, you can type `:PublishReview` to push the
draft review up to github.
