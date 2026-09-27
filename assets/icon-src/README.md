SVG sources for the action/plugin icons in `../icons/`. Not shipped - `build.mjs`
only copies `assets/icons`. Regenerate the 288x288 PNGs after editing:

```bash
cd assets/icon-src && for f in *.svg; do magick -background none -density 300 "$f" -resize 288x288 -strip "../icons/${f%.svg}.png"; done
```
