# Provider icons

The original preset SVG files and the `icons/` picker catalog are copied from CC Switch (`others/cc-switch`, snapshot `846de29`). The picker includes all 110 names in `src/icons/extracted/index.ts`, using its optimized inline SVG strings and locally bundled image files. Display names and search keywords come from `metadata.ts`; entries without metadata keep their icon ID as the label.

The brand artwork originates from LobeHub's [lobe-icons](https://github.com/lobehub/lobe-icons), with additional provider artwork maintained by CC Switch. The original SVG paths are retained. SVG `currentColor` values are replaced with the metadata brand color, except OpenAI uses the official preset's green (`#00A67E`); neutral vectors are tinted by the UI for visibility in both themes. The four existing SwitchX preset icons retain their artwork and tint rules. MiMo's view box and intrinsic dimensions crop unused whitespace around its wordmark. Raster images retain their original bytes inside data URI SVG wrappers so the catalog is bundled offline and uses the same image loader.

The icons identify their respective providers.

## CC Switch

MIT License

Copyright (c) 2025 Jason Young

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Lobe Icons

MIT License

Copyright (c) 2023 LobeHub

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
