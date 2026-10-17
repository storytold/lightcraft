### Updates on macOS install in place
- Install and Relaunch now copies the new version beside the app and swaps it in, puts the old one back if
  anything fails, and starts the new version without opening Terminal or asking for automation access. A copy that
  isn't inside an app bundle (a source build) links to the release page instead of replacing the app in
  /Applications.
