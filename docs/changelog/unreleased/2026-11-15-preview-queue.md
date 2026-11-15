## Library

- **Preview builds queue behind a running build:** a Build Standard-Sized / 1:1 or Smart Previews request made while another build runs is no longer refused — it waits for its turn and starts by itself when the running build ends (identical waiting requests are not queued twice; a runaway queue is capped at 16). Stop Building Previews stops the running build and drops the waiting ones. The full-screen preview (F) now shows the running tasks as a quiet card top right, using the same activity stack. PR #1208.
