  $ enable mutation-norecord amend rebase
  $ setconfig ui.interactive=true amend.safestrip=false hint.ack-hint-ack=true
Test exitting a split early leaves you on the same commit
  $ hg log -r . -T {node}
  d86136f6dbffaed724ce39c03f4028178355246d (no-eol)
  $ hg split << EOF
  > q
  > EOF
  0 files updated, 0 files merged, 2 files removed, 0 files unresolved
  adding d1
  adding d2
  diff --git a/d1 b/d1
  new file mode 100644
  examine changes to 'd1'? [Ynesfdaq?] q
  
  abort: user quit
  [255]
  $ hg log -r . -T {node}
  d86136f6dbffaed724ce39c03f4028178355246d (no-eol)
