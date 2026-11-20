# changelog

What the changelog on [rhue.dev](https://rhue.dev/changelog) reads, and where `CHANGELOG.md` gets generated from (`cargo xtask gen`). One markdown file per release (or milestone, for now), plus an `index.json` that says which ones exist and in what order.

What's merged but not released goes in `unreleased.md`. When it ships, rename it to `vX.Y.Z.md` and add it to the top of `index.json`.

## index.json

Newest first.

```json
{
  "entries": [
    { "file": "m26.md", "tag": "M26", "title": "Hot reload", "version": "0.1.0", "date": "2026-12-01" },
    { "file": "m25.md", "tag": "M25", "title": "Async hosts", "version": "0.1.0" }
  ]
}
```

Only `file` is required, and it has to be a plain name sitting in this folder (no `../`). `tag` falls back to the file name in caps and `title` to the file name. The home page shows the first bullet of each entry as its blurb, set `summary` if that reads badly.

The file name minus `.md` is the URL, so `m25.md` lives at `rhue.dev/changelog/m25`. Don't rename them once they're out.

## The .md files

Plain markdown, no front matter, no title (the page already shows `title`). Same bullets as `CHANGELOG.md` work fine:

```markdown
- Feat: Modules reload while scripts are running. Functions get swapped in place.
- Fix: `require` keeps a module's state across reloads.

### Changed

- `Engine::reload` replaces `Engine::add_module` for modules that are already loaded.
```

Code blocks get highlighted if they say what they are: `rust`, `lua`/`rhue`, `toml`, `json`, `bash`, `powershell`.

## next.json

The "What's next" cards on the home page, in the order they show up.

```json
{ "items": [ { "tag": "M26", "title": "Hot reload", "body": "Reload modules while scripts are running." } ] }
```

`title` is required. `body` can have `inline code` and links. Delete an item once it ships, it'll be in the changelog by then anyway.
