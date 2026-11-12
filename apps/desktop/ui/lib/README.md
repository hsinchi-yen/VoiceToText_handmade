# ui/lib: v2c design system

Material 3 Expressive style, own blue-violet palette. Svelte 5 runes, system fonts, no external resources.
Import from `../lib/index.js` (or a single file, e.g. `../lib/Button.svelte`).

## Tokens (`ui/app.css`)

Use these CSS variables instead of hard-coded colors; light/dark switch by `prefers-color-scheme`.

- Colors: `--md-primary`, `--md-on-primary`, `--md-primary-container`, `--md-on-primary-container`,
  `--md-secondary-container`, `--md-on-secondary-container`, `--md-surface`,
  `--md-surface-container-lowest|low|(none)|high|highest` (`--md-surface-container` is the base one),
  `--md-on-surface`, `--md-on-surface-variant`, `--md-outline`, `--md-outline-variant`,
  `--md-error`, `--md-on-error`, `--md-error-container`, `--md-on-error-container`,
  `--md-success`, `--md-success-container`, `--md-warning`, `--md-warning-container`, `--md-scrim`.
- Radius: `--md-radius-xs|sm|md|lg|xl|full` = 4 / 8 / 12 / 16 / 28 / 9999 px.
- Spacing (4px scale): `--md-space-1|2|3|4|5|6|8|10|12`.
- Type: `--md-type-{display|headline|title|title-sm|body|body-sm|label|label-sm}-{size|line|weight}`.
- Motion: `--md-ease-standard|emphasized|spring`, `--md-duration-short|medium|long` (all durations are 0 under `prefers-reduced-motion`).
- Global helpers: `.hint` (secondary text), `.ok`, `.bad`, `.link` (button that looks like a link). Native
  `button/input/select/textarea/checkbox` get a low-specificity M3 baseline (any component style wins).

## Components

| Component | Purpose | Props |
|-----------|---------|-------|
| `Button` | Action button; link when `href` is set | `variant` filled\|tonal\|outlined\|text, `danger`, `icon`, `disabled`, `href`, `type`, children, other attrs (e.g. `onclick`) |
| `IconButton` | Icon-only button | `icon`, `label` (required, aria-label + tooltip), `variant` standard\|tonal\|filled, `size`, `disabled` |
| `Switch` | On/off toggle (`role="switch"`) | `checked` (bindable), `label` or children, `disabled`, `onchange` |
| `Select` | Styled native select | `value` (bindable), `options` `[{value,label,disabled?}]`, `label`, `supporting`, `disabled`, `onchange` |
| `TextField` | Outlined text input / textarea | `value` (bindable), `label`, `type`, `rows` (>1 = textarea), `supporting`, `error` (string, announced), `disabled`, other input attrs |
| `Card` | Surface container | `variant` filled\|elevated\|outlined, `tag` (default div), `padding`, children |
| `ListItem` | List row | `headline`, `supporting`, `leading` / `trailing` snippets, `href` (link) or `onclick` (button), `selected`, children |
| `Dialog` | Modal dialog (native `<dialog>`: focus trapped, Esc closes, focus restored) | `open` (bindable), `title`, `dismissible` (default true; false blocks Esc and backdrop), `onclose`, children, `actions` snippet |
| `SegmentedButton` | Single choice, 2 to 4 options; always content-sized (`fit-content`), never stretches in grid/flex parents | `value` (bindable), `options` `[{value,label,icon?,disabled?}]`, `label` (group name), `disabled`, `onchange` |
| `Tabs` | Tab bar with roving tabindex and arrow keys | `value` (bindable id), `tabs` `[{id,label,icon?}]`, `idPrefix`, `label`, `onchange`. The caller renders the panel with `role="tabpanel"`, `id="{idPrefix}-panel-{id}"`, `aria-labelledby="{idPrefix}-tab-{id}"` (see `routes/Library.svelte`) |
| `Banner` | Inline message | `variant` info\|success\|warning\|error, `title`, `onclose` (adds a dismiss button), children, `actions` snippet |
| `LinearProgress` | Progress bar | `value` 0..1 (null = indeterminate), `label` |
| `NavRail` | Main menu: left rail (with the v2c logo on top), bottom bar without logo below 600px | `items` `[{path,label,icon}]`, `current`, `label` (used by `App.svelte`) |
| `Icon` | Inline SVG icon | `name`, `size` (24), `label` (omit for decorative) |

Icon names (`icons.js`): home, services, cloud, library, settings, mic, check, error, warning, info,
chevron, chevron-down, close, download, upload, play, stop, pause, plus, trash, copy, edit, refresh,
search, more, link, history, book, keyboard, gpu. Add new ones to `ICONS` (24x24, stroke 2, paths only).

## Routing helpers (`nav.js`, pure and unit-tested)

`NAV_ITEMS` (the 4 menu entries), `parseHash`, `resolveRoute` (old URLs `#/cloud-setup`, `#/vocab`,
`#/history` give a `redirect`), `libraryTab`, `libraryHref`, `activeNavPath`. Menu links must stay
`<a href="#/...">`: the Settings page intercepts such clicks to warn about unsaved changes.

## Dev without Tauri

`npm run dev` in a plain browser loads `ui/dev-mock.js` (dev only, never in `dist/`). It fakes every
`invoke` command and event the UI uses. To mock a new command add one entry to the `commands` table
(name -> function of the args; throw a code string to simulate an error). Console helper:
`__v2cMock.emit("v2c:status", {...})`.
