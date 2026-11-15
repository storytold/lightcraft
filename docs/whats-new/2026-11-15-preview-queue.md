### Preview builds queue behind a running build
- A preview build asked for while another one runs now waits for its turn instead of being refused: the Library ▸ Previews builds stay offered while one runs, and each waiting build starts by itself when the one before it ends (as several processes at once, like Lightroom's queue).
- **Stop Building Previews** stops the running build **and** drops the ones waiting behind it.
- The full-screen preview (F) keeps saying what runs: a quiet card top right with the running task's name and a thin progress bar ("N operations in progress…" over a bar each when several run).
