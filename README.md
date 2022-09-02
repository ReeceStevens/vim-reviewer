# Testing

In a neovim installation with `vim-fugitive` installed, source the `.nvimrc`
local to this repository. Run `:UpdateRemotePlugins`, then re-open vim. Ensure
that there is a local virtualenv `./cli-review-venv`, which has an editable
install of `offline_pr_review` and `requests`.
