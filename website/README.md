# website

The documentation site: [Fumadocs](https://fumadocs.dev) on Next.js.

The pages are the Markdown in the repository (`README.md`, `docs/`,
`examples/README.md`, `CONTRIBUTING.md`). `scripts/sync-docs.mjs` turns
them into MDX under `content/docs/` with a sidebar; `pnpm dev` and
`pnpm build` run it first, and `content/docs/` is not committed. Edit the
Markdown, not the generated pages.

```sh
pnpm install
pnpm dev          # http://localhost:3000
pnpm build        # a production build
```

- `lib/shared.ts`: the site name and, once the repository has a public
  home, the GitHub link.
- `lib/source.ts`: the content source (Fumadocs MDX).
- `app/docs`: the documentation layout and pages; `app/(home)`: the
  landing page; `app/api/search`: search; `app/llms*.txt`: the docs as
  plain text for language models.
