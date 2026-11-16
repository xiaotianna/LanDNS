# LanDNS interface

The LanDNS mark is an L-shaped connection between three network nodes, in off-white and mint on a deep teal rounded square. `public/logo.svg` is the shared source for the sidebar, favicon, and exported macOS app icons in `src-tauri/icons/`. Keep the complete mark and its colors identical across these surfaces.

Astryx's official component previews and application templates are the visual authority. They use the Neutral theme; its Figtree font is provided locally. Follow the single-side-navigation and table-page template compositions, with spacing tokens on layout primitives. Let standard components retain their default typography, colors, and dimensions. Do not add component CSS, theme token overrides, or custom control sizes. Pass icon component references to Astryx icon slots so the library applies the correct size and alignment.

The overview focuses on adding projects and opening their URLs. Project management remains on the project page; DNS diagnostics and network options stay in the collapsed advanced settings. Keep copy concise, state readable as text as well as color, and empty states tied to real data.

Adding and editing a local project use Astryx's default Dialog with a scrollable form and footer actions. Keep entered values on validation or service errors; close after successful saving and activation. Mark required fields with * and leave optional fields unmarked. Keep project field labels concise, with validation feedback when needed.

Toast status colors are the exception to the default-style rule: use Astryx's success/on-success tokens for success, info/on-info for loading, and its default error toast for failures. Preserve Toast dimensions and behavior.

Keyboard access, visible focus, associated input labels, dialog focus management and reduced motion are required. Desktop is the primary surface, with content allowed to wrap at narrower window widths.
