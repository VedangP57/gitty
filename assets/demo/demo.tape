# The README demo: assets/demo/record.sh records it in a made-up repository (no real data).
Output out/demo.gif

Require git
Require gitty

Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 820
Set Padding 12
Set Framerate 24
Set TypingSpeed 60ms
Set Theme "Dracula"

Hide
Type "cd /tmp/skylark && clear" Enter
Show

Type "gitty" Sleep 300ms Enter
Sleep 2.5s

# History: walk down the commits, then flip the diff between split and unified.
Down Sleep 900ms
Down Sleep 900ms
Down Sleep 900ms
Down Sleep 1.2s
Type "s" Sleep 2s
Type "s" Sleep 800ms
Type "g" Sleep 900ms

# Changes: stage the files and commit.
Type "1" Sleep 2s
Down Sleep 1.2s
Down Sleep 1.2s
Type "a" Sleep 1.2s
Type "c" Sleep 600ms
Type "Show how cold the wind makes it feel" Sleep 500ms
Enter
Type "Adds wind chill to the forecast line." Sleep 800ms
Alt+Enter Sleep 2s

# Back in History, the new commit is on top.
Type "2" Sleep 1.2s
Type "g" Sleep 2.5s
Type "q" Sleep 500ms
