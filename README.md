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
draft review up to GitHub or GitLab.

### GitHub vs GitLab Support

The plugin automatically detects whether your repository is hosted on GitHub or
GitLab based on the remote URL. Both SSH and HTTPS URLs are supported:

- **GitHub**: `git@github.com:owner/repo.git` or `https://github.com/owner/repo.git`
- **GitLab**: `git@gitlab.com:owner/repo.git` or `https://gitlab.com/owner/repo.git`

### Authentication

The plugin requires an API token to publish reviews. There are two ways to provide authentication:

#### Option 1: Configuration File (Recommended)

Create a `vim-reviewer.toml` file in your project's root directory with the following format:

```toml
[backend]
type = "gitlab"  # or "github"
url = "https://gitlab.example.com"  # optional - defaults to detecting from git remote
token = "your-api-token-here"
```

The plugin will automatically detect this file and use the token specified. The `url` field is optional - if omitted, the plugin will detect the repository URL from your git remote.

**Example for GitHub:**
```toml
[backend]
type = "github"
token = "ghp_xxxxxxxxxxxxxxxxxxxx"
```

**Example for GitLab with custom URL:**
```toml
[backend]
type = "gitlab"
url = "https://gitlab.example.com/owner/repo"
token = "glpat-xxxxxxxxxxxxxxxxxxxx"
```

#### Option 2: Environment Variables

Alternatively, you can set environment variables:

- **GitHub**: Set the `GH_REVIEW_API_TOKEN` environment variable with your GitHub personal access token
- **GitLab**: Set the `GITLAB_TOKEN` environment variable with your GitLab personal access token

For GitHub, create a token at https://github.com/settings/tokens with the `repo` scope.

For GitLab, create a token at https://gitlab.com/-/user_settings/personal_access_tokens with the `api` scope.

**Note:** The configuration file takes precedence over environment variables and git remote detection.

**Security Warning:** If you use the `vim-reviewer.toml` configuration file, make sure to add it to your `.gitignore` to avoid accidentally committing your API token to version control:

```bash
echo "vim-reviewer.toml" >> .gitignore
```

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
I can leave comments directly in vim, then publish them all to GitHub or GitLab as a draft
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
