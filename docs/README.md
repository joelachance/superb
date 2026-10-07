# docs

Everything documentation lives here: the Markdown, and the site that
serves it.

The Markdown is the source. `flows.md` and `flows/<flow>.md` cover the
flow tier, `primitives.md` and `primitives/<module>.md` the engine,
`ARCHITECTURE.md` the rules and `TERMINOLOGY.md` the words; the
repository's `README.md`, `examples/README.md` and `CONTRIBUTING.md`
complete the set. GitHub renders all of it as it is.

The site is [Fumadocs](https://fumadocs.dev) on Next.js. `scripts/sync-docs.mjs`
turns that Markdown into MDX under `content/docs/` with a sidebar;
`pnpm dev` and `pnpm build` run it first, and `content/docs/` is not
committed. Edit the Markdown, not the generated pages.

```sh
cd docs
pnpm install
pnpm dev          # live at http://localhost:3000, rebuilds pages as you edit
pnpm build        # a production build
pnpm start        # serve the production build
```

- `lib/shared.ts`: the site name and, once the repository has a public
  home, the GitHub link.
- `lib/source.ts`: the content source (Fumadocs MDX).
- `app/docs`: the documentation layout and pages; `app/(home)`: the
  landing page; `app/api/search`: search; `app/llms*.txt`: the docs as
  plain text for language models.
