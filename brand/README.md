# Lo brand

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo/lockup-cream.svg">
  <img alt="Lo" src="logo/lockup-ink.svg" height="56">
</picture>

The mark, the wordmark and the few colors and type settings around them. There are no components here on purpose.

Paths in this file are relative to this folder, `brand/`. From the repository's root, they start with `brand/`: `logo/mark.svg` is `brand/logo/mark.svg`.

## The name

Write it **Lo**, always with a capital L, wherever people read it. A lowercase l looks like a capital I in most sans-serif fonts, so "lo" reads as "Io".

Lowercase `lo` appears only as code, such as the terminal command, the Homebrew name, the repo and `lo://` links. "The name" in [`CLAUDE.md`](../CLAUDE.md#the-name) has the full list.

## The mark

An L holding a ball that is lifted out of its socket. It is one shape in one color.

| File | Use |
| --- | --- |
| [`logo/mark.svg`](logo/mark.svg) | Takes the surrounding text color (`currentColor`) |
| [`logo/mark-cream.svg`](logo/mark-cream.svg) | On dark backgrounds (the default) |
| [`logo/mark-ink.svg`](logo/mark-ink.svg) | On light backgrounds |
| [`logo/mark-black.svg`](logo/mark-black.svg) | The menu bar, set as a template image so macOS tints it |
| [`logo/png/mark-*.png`](logo/png/) | 512 px exports, for places that won't take SVG |

**Clear space.** Keep the ball's radius free on every side: 21 units of the mark's 76, a little over a quarter of its height. The PNG exports have no margin, so add it where you place them.

**Minimum size.** 16 px for the mark, and 20 px tall for the lockup. No separate small version is needed.

**Don't:** seat the ball in the socket, color the ball, outline the mark, rotate or flip it, change its proportions, stretch it, or add shadows, glows or gradients. Keep it on ink or cream, or on whatever background it reads clearly against.

## Wordmark and lockup

The wordmark is the name set in Host Grotesk ExtraBold, tracked in by 2%, and converted to shapes so it needs no font installed. The lockup puts the mark and wordmark side by side. The gap between them is one ball radius, and the wordmark's cap height is two thirds of the mark's height.

| File | Use |
| --- | --- |
| [`logo/lockup.svg`](logo/lockup.svg), [`logo/wordmark.svg`](logo/wordmark.svg) | Take the surrounding text color (`currentColor`) |
| [`logo/lockup-cream.svg`](logo/lockup-cream.svg), [`logo/wordmark-cream.svg`](logo/wordmark-cream.svg) | On dark backgrounds |
| [`logo/lockup-ink.svg`](logo/lockup-ink.svg), [`logo/wordmark-ink.svg`](logo/wordmark-ink.svg) | On light backgrounds |
| [`logo/social-preview.svg`](logo/social-preview.svg), [`logo/png/social-preview.png`](logo/png/social-preview.png) | The repo's social preview: the cream lockup on ink, 1280 × 640 |

## The README banner

[`banner/banner-dark.svg`](banner/banner-dark.svg) (cream on ink) and [`banner/banner-light.svg`](banner/banner-light.svg) (ink on cream) head the repository's [`README.md`](../README.md). Each is 1600 × 480, with the lockup at three times its size in the middle and thin corner ticks in the secondary text color, at half opacity.

They're animated SVGs, which GitHub plays in a README. The banner plays once, in about two seconds, and rests: the L fades in, the ball rises out of its socket into its lifted place, and the wordmark slides in. The ball starts well clear of the socket, so it never seats, and nothing rotates, flips, stretches, glows or changes color. Each element's own style is the final frame, so with reduced motion turned on, or anywhere CSS animation doesn't run, the banner shows the finished lockup. Change both files together, and keep the lockup's shapes the same as `logo/lockup.svg`.

The banner follows the reader's theme. From the repository's root:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/banner/banner-dark.svg">
  <img alt="Lo" src="brand/banner/banner-light.svg" width="100%">
</picture>
```

## Where each asset goes

| Place | Use |
| --- | --- |
| README header | The banner, `banner/banner-dark.svg` on dark and `banner/banner-light.svg` on light (snippet above) |
| Other Markdown headers, such as this file's | `logo/lockup-cream.svg` on dark, `logo/lockup-ink.svg` on light (snippet below) |
| getlo.dev header (planned) | Lockup |
| Repo social preview | `logo/png/social-preview.png`, exported from `logo/social-preview.svg`; upload it in the repo's settings |
| GitHub sign-in | The mark alone; GitHub shows the name beside it |
| Menu bar | `logo/mark-black.svg` as a template image (see "To come") |
| Window titles | The name as text, no mark |

A Markdown header that follows the reader's theme, from the repository's root (this file uses the same paths without `brand/`):

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/logo/lockup-cream.svg">
  <img alt="Lo" src="brand/logo/lockup-ink.svg" height="56">
</picture>
```

## Color

Ink and cream are fixed. The accent is for the interface only: links, focus rings, selection and the main button. It never goes in the mark.

| | Dark (default) | Light |
| --- | --- | --- |
| Ground | `#15161D` ink | `#F4ECDE` cream |
| Text and mark | `#F4ECDE` cream | `#15161D` ink |
| Secondary text | `#A7A398` stone | `#66625A` umber |
| Rules and borders | `#33353D` | `#DCD2C1` |
| Accent | `#A5C5E9` powder | `#0664B2` deep powder |
| Text on accent | `#15161D` | `#F4ECDE` |

Every pairing of text and background meets WCAG AA contrast. Keep green, red and purple for diffs and pull request status, as GitHub uses them.

The same values are in [`tokens/brand.css`](tokens/brand.css) (CSS custom properties, dark by default, light through `prefers-color-scheme` or `data-theme="light"`) and [`tokens/brand.json`](tokens/brand.json).

## Type

- **Host Grotesk** for text, at weights 400 to 800.
- **Fragment Mono** for code.

This is the same pairing as scornz, so the two brands feel related. Both fonts are free under the SIL Open Font License, from Google Fonts ([Host Grotesk](https://fonts.google.com/specimen/Host+Grotesk), [Fragment Mono](https://fonts.google.com/specimen/Fragment+Mono)). Use them for the README, getlo.dev (planned) and the sign-in screen. GitHub sets a README's text in its own fonts, so there the banner's wordmark carries the type. Inside the app, the system font keeps Lo feeling native.

| Style | Size / line height | Weight |
| --- | --- | --- |
| Display | 48 / 1.05 | 800 |
| Title | 28 / 1.2 | 700 |
| Body | 17 / 1.5 | 400 |
| Small | 14 / 1.4 | 400 |
| Code | 14 / 1.5, mono | 400 |

## To come

Made later, here in `brand/`, from the mark and these rules:

- **The app icon.** `Lo.icns`, and the iconset it's built from (`Lo.iconset/`, 16 to 512 pt at @1x and @2x), drawn on macOS's app icon grid with its padding around the rounded square.
- **The menu-bar icon.** The mark as a template image, black on transparent, at @1x and @2x, so macOS tints it for light and dark menu bars.
- **The DMG background.** The window behind the app and the Applications folder when the disk image opens, at @1x and @2x.
