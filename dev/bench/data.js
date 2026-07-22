window.BENCHMARK_DATA = {
  "lastUpdate": 1784705376773,
  "repoUrl": "https://github.com/w0wl0lxd/n00n",
  "entries": {
    "Criterion": [
      {
        "commit": {
          "author": {
            "email": "w0wl0lxd@tuta.com",
            "name": "w0wl0lxd",
            "username": "w0wl0lxd"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "a8a0575602abc7b06c1f812edb03c99d19b2fa86",
          "message": "Merge pull request #48 from w0wl0lxd/add-strict-lint-rules\n\nrefactor: complete workspace strict lint cleanup",
          "timestamp": "2026-07-22T03:16:00-04:00",
          "tree_id": "861b7d75873f7f18d837e19e313f4d15f7cbe27f",
          "url": "https://github.com/w0wl0lxd/n00n/commit/a8a0575602abc7b06c1f812edb03c99d19b2fa86"
        },
        "date": 1784705376027,
        "tool": "cargo",
        "benches": [
          {
            "name": "fib/jit_mlua_hook",
            "value": 6415197,
            "range": "± 182192",
            "unit": "ns/iter"
          },
          {
            "name": "fib/jit_watchdog",
            "value": 2487860,
            "range": "± 47834",
            "unit": "ns/iter"
          },
          {
            "name": "fib/jit_none",
            "value": 2484238,
            "range": "± 69604",
            "unit": "ns/iter"
          },
          {
            "name": "fib/interp_mlua_hook",
            "value": 7499415,
            "range": "± 77115",
            "unit": "ns/iter"
          },
          {
            "name": "fib/interp_watchdog",
            "value": 3764537,
            "range": "± 12704",
            "unit": "ns/iter"
          },
          {
            "name": "fib/interp_none",
            "value": 3666072,
            "range": "± 8049",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/jit_mlua_hook",
            "value": 554490,
            "range": "± 1894",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/jit_watchdog",
            "value": 168132,
            "range": "± 1309",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/jit_none",
            "value": 169282,
            "range": "± 236",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/interp_mlua_hook",
            "value": 1062944,
            "range": "± 13715",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/interp_watchdog",
            "value": 619394,
            "range": "± 9012",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_rw/interp_none",
            "value": 619842,
            "range": "± 12041",
            "unit": "ns/iter"
          },
          {
            "name": "splash_render_120x40",
            "value": 74268,
            "range": "± 2501",
            "unit": "ns/iter"
          },
          {
            "name": "splash_render_200x60",
            "value": 102008,
            "range": "± 13695",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}