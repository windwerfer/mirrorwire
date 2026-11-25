# Store metadata (fastlane layout)

`fastlane/metadata/android/en-US/` is read by F-Droid and IzzyOnDroid
(<https://f-droid.org/docs/All_About_Descriptions_Graphics_and_Screenshots/>).

- `title.txt`, `short_description.txt` (80 characters at most), `full_description.txt`
- `images/icon.png` (copy of `packaging/icons/io.github.windwerfer.MirrorWire.png`, 512x512)
- `changelogs/<versionCode>.txt` (500 bytes at most; the file name is the versionCode, see
  `packaging/fdroid/README.md`; add `changelogs/<code>.txt` for each release)

**Screenshots are missing on purpose.** The repository has no real screenshots, and none were
made up. Thomas: take 2 to 8 PNG or JPEG screenshots on a phone while mirroring a real screen
and put them in `en-US/images/phoneScreenshots/` (`1.png`, `2.png`, ...). Optionally add
`images/featureGraphic.png` (1024x500). Without screenshots the listing still works.
