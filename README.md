# Shell
Making the terminal application I want to use.

[![Docs](https://docs.rs/dynamics/badge.svg)](https://www.athanorlab.com/docs)

A simple shell; automates the problems I have trouble with in normal shells. Good autocomplete. Good
knowledge of what folders are commonly used. Less typing for my workflows. Automate the boring
parts of terminals. Compatible with Windows, Linux, and Mac. Windows users probably need to have
Powershell 7 or higher installed.


### Todo: Do we want to supplement or replace this program with a GUI version instead? 
It woulds be a combination of file manager and CLI system; or a general-purpose way of executing OS-level etc
CLI commands, but in 2 dimensions.


Highlights:

- Syntax highlighting
- Directory bookmarks
- Intuitive autocomplete
- Shortcuts for common workflows, e.g. with git.


## Example use

### Using directory bookmarks

Saving bookmarks
```sh
```

#### Loading bookmarks

Type `cd`, then a few characters from the folder name, then press tab to complete the bookmark.


### Autocomplete 

### Git assistance
Run `sync` followed by a commit message in quote. Quotes are optional. This runs the following:
  - `git add .`
  - `git commit -am <the commit message>`
  - `git push`

```shell
sync "A commit message"

// Or:

sync A commit message
```

Warning: This isn't suitable for all workflows. If you use git in a way where it isn't appropriate to sync all gitignored files, this may have unintended consequences!


### Typed commands
- `exit` or `quit`: Exit the program.
- `sync`: Run `git add .`, `git commit -am <the commit message>`, and `git push`.
- `del bm <number>`: Delete a bookmark by number. 
- `his <number>`: Execute a command from history.
- `cat`: Displays the contents of a (generally text) file. Similar to the standard Linux operation, but
also works on Windows.
- `cd <number>`: Go to this recent directory (As listed with Ctrl + R)
- `bm <number>`: Go to this bookmark (As listed with Alt + B)
- `cd <part-of-path>` + Tab key: Go to this directory history item

## Key commands
- Enter key: Send input
- Arrow keys:
- Tab key: while using with cd, autocompletes, including to bookmarks.

### Recent or frequent commands
``
- Ctrl + B: Bookmark the current directory.
- Ctrl + R: List the most recent directories a command has been executed from.
- Ctrl + H: List the most recent items from history.

- Alt + B: List all bookmarks.
- Ctrl + D: Exit



## Application state
Application state, including folder bookmarks, is saved in a file called `shell_state.ss`, in the user's
home directory. It is small, typically a few tens of kb.
