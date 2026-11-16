## Fixed
- Android: opening a 200 MP photo made the system close the app (out of memory). A photo too big to decode whole within the memory budget is decoded at a half, a quarter or an eighth of its size for the loupe and the crop view, and is not decoded ahead of a zoom; exports and CLI renders still read it whole.
- Android: Semantic search (background AI indexing) was on by default; it is now off until switched on, and an installed search model is not loaded at startup while search is off (on every platform).
- Android: the background preview build decoded each photo whole even for a standard preview, two photos at a time; it now reads each only as large as its preview needs, and builds one photo at a time under a small memory budget.
- Touch layout: a tap on a photo in the grid sometimes opened the photo opened last; it now opens the photo under the finger.
