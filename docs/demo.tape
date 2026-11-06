# Offline demo: diff a mocked received set against the recorded Helius reference.
# Render with charmbracelet/vhs: vhs docs/demo.tape
Output docs/demo.gif
Set FontSize 14
Set Width 1400
Set Height 700
Type "cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json"
Enter
Sleep 4s
Type "echo $?"
Enter
Sleep 2s
