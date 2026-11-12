# The Astrum website

The site at <https://robert-mcdermott.github.io/astrum/> is made from this
folder by `site/build.mjs`. It needs Node 18 or newer and no packages, and
nothing here is part of Astrum's own build.

```
site/
  build.mjs          the generator: pages in, a finished site out, everything checked
  markdown.mjs       the Markdown the documentation is written in
  roff.mjs           the manual page, from doc/astrum.1
  menu.json          the documentation menu: its sections and the order of the pages
  pages/index.html   the front page
  pages/404.html     the page for a link that leads nowhere
  pages/docs/*.md    the documentation, a page per subject
  assets/            the stylesheet, the script, the logos
  images/            the screenshots, a light and a dark version of each
  capture/           what takes the screenshots
```

## Building and looking at it

Build Astrum first: the generator asks the binary for the lists it shows
(every key, every setting, every theme, the version and the manual page), so
they are always those of the Astrum beside them.

```bash
cargo build --release
```

```bash
node site/build.mjs --serve
```

That builds the site into `target/site` and serves it at
<http://127.0.0.1:8977/astrum/> (`--port` picks another port), the same path GitHub Pages serves it under.
`node site/build.mjs` alone only builds, and `--out DIR` builds somewhere
else. The build stops with a list of what is wrong if a local link, an anchor
or a picture leads nowhere, a picture has no description, a page has more or
less than one main heading, a heading skips a level, something would be
loaded from another site, or the colors of the site's text are too faint to
read.

## Changing a page

Each page holds only its own words. The generator adds the header, the
menu, the "On this page" list, the links to the pages before and after, and
the footer.

A documentation page starts with its title and a one-sentence description,
which is shown under the title, used for search engines and listed on the
documentation's first page:

```markdown
---
title: Tabs and panes
description: Split a tab into panes, move between them, and keep it all tidy.
---

## Opening a tab

Click `+` in the tab bar…
```

The page's title is its only first-level heading, so the page itself starts
at `##`. What the Markdown can do:

- headings `##` to `####`, each with an id made from its text (or given:
  `## A heading {#an-id}`), which links can point at: `agent.html#autonomy`
- paragraphs, `-` lists and numbered lists, one level of nesting (two spaces)
- fenced code blocks with a language (`bash`, `toml`, `text`)
- tables, with `|`, and `\|` for a bar inside a cell
- `> ` for a note
- `` `code` ``, `**bold**`, `*emphasis*`, `[links](other-page.html)`, and
  `<kbd>Ctrl+B</kbd>` for keys
- a line starting with a block tag, such as `<figure>` or `<div>`, is taken
  as HTML up to the next blank line (one starting with `<kbd>` is ordinary text)

Links to other documentation pages are written as the page's file name with
`.html` (`settings.html#the-agent`); the build checks each one.

To add a page, write `site/pages/docs/<name>.md` and put `<name>` in
`site/menu.json` where it belongs.

### What Astrum makes

These lines are replaced with what the Astrum binary says, so they cannot
fall behind it. Each stands on a line of its own:

| Line | Becomes |
| --- | --- |
| `{{keys}}` | every action, what it does, and its keys |
| `{{settings}}` | every setting in the config file, its default and what it does |
| `{{sample}}` | the starter config file, as `astrum config --init` writes it |
| `{{themes}}` | every built-in theme, drawn in its own colors |
| `{{manual}}` | the manual page |
| `{{changelog}}` | the changelog |
| `{{install}}` | the one-line install, with a button to copy it |

`{{version}}` can also stand inside a sentence.

### Screenshots

```markdown
{{shot panes | Two tabs, the first split into three panes | A tab split three ways}}
```

That is the screenshot's name, a description of what it shows for anyone
who cannot see it, and a caption. The page shows the light picture or the
dark one, whichever matches how it is being viewed.

## Taking the screenshots again

The screenshots are drawn by Astrum: each is a scripted session in a
sandbox of its own, with a stand-in model that always answers the same way,
drawn by the headless client and photographed by headless Chrome. So every
run gives the same pictures, and they can be taken again whenever the look
of Astrum changes.

```bash
node site/capture/capture.mjs
```

It needs a release build and Google Chrome (or Chromium; `CHROME=/path`
names one). `--only panes,menu` takes only those. It writes
`site/images/<name>-light.webp` and `<name>-dark.webp`, plus the picture
shown when a link to the site is shared, and the images used by the
repository's README. The scenes are in `site/capture/scenes.mjs`: what is
typed and clicked, and what the stand-in model answers. The sandbox has its
own home, config, data and runtime folders, so nothing of yours is read or
changed.

## Publishing

The site is served from the `gh-pages` branch, which holds only the built
site. It is checked out beside this repository:

```bash
git worktree add --orphan -b gh-pages ../astrum-site
```

(once; after that the folder is there). In each release's preparation:

```bash
node site/capture/capture.mjs
```

```bash
node site/build.mjs --publish
```

`--publish` builds into `../astrum-site` after checking that it is the
`gh-pages` worktree, and replaces what was there. Then commit and push from
that folder, once the release is out:

```bash
git -C ../astrum-site add -A
```

```bash
git -C ../astrum-site commit -m "Site for 0.6.0"
```

```bash
git -C ../astrum-site push origin gh-pages
```

GitHub Pages is set to serve the `gh-pages` branch from its root.
