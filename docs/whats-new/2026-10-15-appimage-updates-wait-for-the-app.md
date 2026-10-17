### AppImage updates wait for the app to quit
- The new version starts once the old one has quit, so it no longer finds the library still open. The previous
  AppImage, kept until then in case the swap fails, is deleted on the next start.
