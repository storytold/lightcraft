---
title: Does closing the window quit LightCraft?
description: On a Mac, closing the LightCraft window with the red button or ⌘W quits the app, after it checks for running tasks and unsaved changes.
order: 30
verified: 0.14.0
---

On a Mac, yes. Closing LightCraft's main window quits the app, the same as **LightCraft** > **Quit LightCraft** (⌘Q). Many Mac apps keep running with no window open; LightCraft doesn't.

These all do the same thing when the main window is the one in front:

- the red close button in the top-left corner of the window
- **File** > **Close Window** (⌘W)
- **LightCraft** > **Quit LightCraft** (⌘Q)

## Does it ask first?

Only when something would be lost:

- A task that can be stopped is still running, such as an import or an export. LightCraft asks **Quit while tasks are running?** Choose **Cancel** to keep working, or **Quit Anyway** to stop the task and quit.
- Some changes couldn't be written to disk, for example because a drive was unplugged. LightCraft tries to save them again, and if that still fails it asks **Quit with unsaved changes?** Choose **Cancel** to stay, **Try Saving Again** once the drive is back, or **Quit Anyway** to give up those changes.

With nothing running and everything saved, the window closes and LightCraft quits at once. As the app quits, LightCraft saves your settings and closes your library. If either save fails, the error goes to the log file and the app quits anyway.

The second window (**Window** > **Second Window**) is different: its red button, or ⌘W while it is in front, closes only that window, and LightCraft keeps running.

## Can I keep LightCraft open without a window?

No. To get the window out of the way, minimize it with the yellow button or **Window** > **Minimize** (⌘M), or hide the app with **LightCraft** > **Hide LightCraft** (⌘H). Both keep your place and any running task going.
