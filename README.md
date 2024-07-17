# LogGlance

A simple log viewer made to view and tail large log files.  
Can easily handle 1GiB files, after that the memory overhead can be a bit cumbersome.
Files can therefore be opened in "Restricted mode" to only load the last 4GiB up to 120 million rows to keep memory usage lower.

I created this project to learn how to use egui in a multi-threaded desktop application and make a tool available for me that can handle parts of larger files without resorting to head/tail/less and the like.

## TODO: Write som good stuff in here.

