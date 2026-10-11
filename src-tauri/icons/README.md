# PromLex · 语锚 application icon

The source artwork is `../../public/app-icon.png`. It was generated with the built-in imagegen tool with a transparent background, then inspected for the kitten, feather, and the exact character **译**.

Regenerate the platform icons from the repository root:

```sh
npm run tauri -- icon public/app-icon.png
```

Generation prompt:

```text
Use case: logo-brand
Asset type: square desktop application icon for PromLex (Chinese name 语锚).
Primary request: a cute small kitten standing on a large feather, with the Simplified Chinese character "译" written clearly on the feather.
Style: polished charming flat illustration, crisp clean outlines, soft restrained shading, readable silhouette at small icon sizes.
Composition: centered single kitten above a broad gently curved feather, full silhouette fits within square canvas with generous transparent margins. Feather must have a broad visible surface beneath the kitten, featuring one large legible dark character.
Text (verbatim): "译". Exactly this one Chinese character prominently on the feather; no other text.
Constraints: truly transparent background; no frame, no watermark; cute expressive face; paws visibly standing on feather. 1024x1024 square.
```
