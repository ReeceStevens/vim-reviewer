# `vim-reviewer`: Offline-first Reviews in Neovim

`vim-reviewer` is a plugin for performing code reviews from within neovim.

This is still _very early stage alpha_-- I am developing this primarily for my
own use case, which is GitHub pull request reviews. There are still many sharp
edges, and the interface could change at any moment. That being said... you
_can_ currently use it to do PR reviews.

**This plugin's development was started as a part of [Innolitics 10x
Time](https://innolitics.com/10x/time/)**

## Demo

https://user-images.githubusercontent.com/5847947/193730037-616d7b20-2d34-430f-a680-a23e50bea87f.mov

## Dependencies

This plugin requires:

1. `neovim`
2. `vim-fugitive`

Within the plugin is a Python module that depends on the `requests` python
package.

## Installation

_Note: this install process is not ideal due to all the little details of
getting the python environment working. This will be an area of future work for
this plugin._

With `vim-plug`:

```vim
Plug 'ReeceStevens/vim-reviewer'
```

A Python3 virtualenv must be created for this plugin to be installed. I
recommend the following approach:

1. Create a dedicated Python 3.7 or up virtualenv in `~/.vim`

2. Source that virtualenv and install `pynvim`

3. Set that virtual env as your host python for neovim: 

```
let g:python3_host_prog = $HOME . '/.vim/python-virtual-env/nvim-venv/bin/python'
```

4. Pull down the repository with `:PlugInstall`

5. Install the python module included with the plugin (with virtualenv
   activated):

```
pip install -e ~/.vim/plugs/vim-reviewer/offline_pr_review
```

## Usage

Open a file in a git repository and run `:StartReview <pr-number>`-- for
example, `:StartReview 1`.

After that, navigate to the files you want to review. Leave a comment on a
single line or a range by using `:ReviewComment`.

Type your comment into the buffer, then save and exit. `:EditComment` and
`:DeleteComment` can be used to edit or delete the comment under the cursor,
respectively.

Similarly, you can use the `:ReviewBody` command to fill out the body of a PR
review.

Once you're done leaving comments, you can type `:PublishReview` to push the
draft review up to github.

## Internals

This plugin creates a JSON file in the git dir of the repository you're working
in. It will create a `.git/reviews` directory, under which all review files will
be saved.

Until you use `:PublishReview`, nothing is sent to GitHub. The review is just
saved locally in the JSON file.

## Why I Built This

For most non-trivial PRs, I like to perform reviews locally in my editor. My
typical workflow looks something like this:

```bash
$ git diff --stat origin/main...HEAD | vim
```

From there, I convert the diff stat into a checklist:

```
- [ ] offline_pr_review/offline_pr_review/offline_pr_review.py |  4 ++++
- [ ] rplugin/python3/nvim-plugin.py                           | 31 +++++++++++++++++++++++++------
 2 files changed, 29 insertions(+), 6 deletions(-)
```

and I manually enter my comments as sub-bullets below the file, along with the
line number. Once I finish my review, I have to open up GitHub and copy over my
comments to the right spot.

The first pain point this plugin is meant to solve is this last copy step-- now,
I can leave comments directly in vim, then publish them all to github as a draft
review.

## FAQ

### Why neovim and not vim?

Mostly, due to ease of development. This plugin takes advantage of neovim's RPC
system and is primarily written in Python.

# Testing

In a neovim installation with `vim-fugitive` installed, source the `.nvimrc`
local to this repository. Run `:UpdateRemotePlugins`, then re-open vim. Ensure
that there is a local virtualenv `./cli-review-venv`, which has an editable
install of `offline_pr_review` and `requests`.
