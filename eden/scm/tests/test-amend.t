#testcases obsstore-off obsstore-on mutation
#endif
#if obsstore-off
#if mutation
  $ enable mutation-norecord
#endif
#endif
#if mutation

  $ hg log -T '{rev} {node|short} {desc}\n' -G
  @  3 be169c7e8dbe B
  |
  | o  2 26805aba1e60 C
  | |
  | x  1 112478962961 B
  |/
  o  0 426bada5c675 A
  