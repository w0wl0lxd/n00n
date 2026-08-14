`grep`'s "not supported" hint now also calls out backreferences (`\1`, `\k<name>`), not just look-around, matching the other common PCRE habit that trips up Rust regex.
