# Fontique 0.11.1 backport

Published Fontique 0.11.1, with one source change: backport the empty Fontconfig
font-set fix from [upstream commit 4bb654b](https://github.com/linebender/parley/commit/4bb654bbb4328e2a11374655750493b5a4671128).
Skip generic families when `font_sort` returns `NoMatch`, rather than panicking.
Remove the patch when a compatible published release contains the fix.

Original source revision: `eea3503dd6cf17130cbb07348e0ff2c918300e94`.
The accompanying MIT and Apache-2.0 licences are unchanged.

Cargo registry packages omit path patches. Before publishing Codestrand on
crates.io, obtain a compatible published Fontique backport and remove the patch.
Binary releases and the Python source distribution retain the backport.
