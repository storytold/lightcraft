## Performance

- **Publishing from a large library:** a publish built one copy of the library's originals index per photo and kept them all until the run ended, so memory and time grew with photos x library size (a 10,000-photo publish from a 10,000-photo library passed 50 GB before sending anything). A run now builds the index once and shares it, as export does (#1375). Thanks @blwfish.
