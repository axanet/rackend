# rackend admin UI: design notes

The visual identity is defined in the vault note **17 Brand/Visual Identity**
(normative) and implemented by the homepage tokens in
`rackend-homepage/src/styles/global.css`. Copy follows **17 Brand/Writing Style**.
This file only records how the admin UI applies them. It does not define
colours or fonts of its own.

## Where things live

- Tokens and primitives: `admin-ui/src/app.css` (token values copied from the
  homepage; keep them in sync in the same change).
- Icons: `admin-ui/src/lib/components/Icon.svelte` (16px grid, 1.4px stroke).
- Mark and wordmark: `Logo.svelte` (placeholder; do not build on it).
- Theme: `ThemeToggle.svelte` cycles system, light, dark; the head script in
  `admin-ui/src/app.html` applies a stored choice before first paint.
- Server-rendered pages (error page, "UI not built" placeholder):
  `crates/http/src/html.rs` (`PAGE_CSS`) and `crates/rackend/build.rs`.

## Admin-specific choices

- Density: tables use 40px rows and mono 11px uppercase headers on `--surface`.
  Forms use 40px inputs with 10px radius.
- Buttons: 44px pills for page actions (`.btn`, `.btn.primary` is solid `--fg`).
  Toolbars, table rows and the sidebar use the 34px variant (`.btn.sm`), like
  the homepage header.
- Signal colours: `--accent` only for primary, success and the active item;
  `--danger` for errors and destructive actions; `--write` for data-safety
  warnings (public update/delete rules, newly required fields).
- Mono (Geist Mono) for ids, timestamps, numbers, field and collection names,
  rule expressions, SQL and labels.
- Empty states are dashed placeholders ("not here yet"). Empty table cells say
  `none` in `--fg-subtle`.
- Cards: `--bg-elev`, 1px `--border`, a 30px icon tile plus a mono label in the
  header row; the border goes `--border-strong` on hover and nothing moves.
- Dialogs are the only elements with a shadow (`--shadow-lg`, 16px radius).

## Checks before shipping UI

- Both themes, at 375px and desktop width.
- WCAG AA contrast; `--accent-text` for small green text on light.
- Visible focus ring (2px `--accent`, 2px offset); keyboard reachable.
- No em dashes (U+2014): see the check command in **17 Brand/Writing Style**.
