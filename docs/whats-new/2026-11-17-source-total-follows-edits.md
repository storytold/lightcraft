### Rating a photo in a big view no longer stalls the grid
- The photo count shown beside the source's name was worked out again from every photo in the library after each
  edit, so in a very large library each rating, flag or label in a large smart album or folder was slow to show.
  After an edit it now asks only the photos the edit touched, the way the grid's list already did; an edit to many
  photos at once, to an album's rules or photos, Recently Added, or a source that reads the clock is still counted
  from every photo. Opening a source with nothing narrowing it takes the count from the pass that builds the grid.
