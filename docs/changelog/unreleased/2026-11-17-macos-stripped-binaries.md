## Release builds and CI

- **Smaller macOS downloads:** the app and the CLI in the macOS disk images are stripped of their symbol tables before signing, as the Linux and FreeBSD packages already were: about 40 MB less per program and architecture, and about 11 MB less to download for the Apple silicon image. Crash logs still record each panic's message and file:line (issue #1227).
