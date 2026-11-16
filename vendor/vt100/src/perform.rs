use alloc::vec::Vec;

const BASE64: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";

impl<CB: crate::Callbacks> vte::Perform for crate::screen::WrappedScreen<CB> {
    fn print(&mut self, c: char) {
        // tmux shows a byte that is not valid UTF-8, or a character it cannot measure such
        // as a C1 control, as U+FFFD (input_stop_utf8)
        if ('\u{80}'..'\u{a0}').contains(&c) {
            self.text('\u{fffd}');
        } else {
            self.text(c);
        }
    }

    fn execute(&mut self, b: u8) {
        match b {
            7 => self.callbacks.audible_bell(&mut self.screen),
            8 => self.bs(),
            9 => self.tab(),
            10 => self.lf(),
            11 => self.vt(),
            12 => self.ff(),
            13 => self.cr(),
            // we don't implement shift in/out alternate character sets, but
            // it shouldn't count as an "error"
            14 | 15 => {}
            // vte passes a lone byte from 0x80 to 0x9f here; tmux has no C1 controls and
            // shows it as an invalid UTF-8 byte
            0x80..=0x9f => self.text('\u{fffd}'),
            _ => self.callbacks.unhandled_control(&mut self.screen, b),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, b: u8) {
        if intermediates == b"#" && b == b'8' {
            self.decaln();
        } else if let Some(i) = intermediates.first() {
            self.callbacks.unhandled_escape(
                &mut self.screen,
                Some(*i),
                intermediates.get(1).copied(),
                b,
            );
        } else {
            match b {
                b'7' => self.decsc(),
                b'8' => self.decrc(),
                b'=' => self.deckpam(),
                b'>' => self.deckpnm(),
                b'H' => self.hts(),
                b'M' => self.ri(),
                b'c' => {
                    self.ris();
                    self.callbacks.reset(&mut self.screen);
                }
                b'g' => self.callbacks.visual_bell(&mut self.screen),
                _ => {
                    self.callbacks.unhandled_escape(
                        &mut self.screen,
                        None,
                        None,
                        b,
                    );
                }
            }
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        let unhandled = |screen: &mut Self| {
            screen.callbacks.unhandled_csi(
                &mut screen.screen,
                intermediates.first().copied(),
                intermediates.get(1).copied(),
                &params.iter().collect::<Vec<_>>(),
                c,
            );
        };
        match intermediates.first() {
            None => match c {
                '@' => self.ich(canonicalize_params_1(params, 1)),
                'A' => self.cuu(canonicalize_params_1(params, 1)),
                'B' => self.cud(canonicalize_params_1(params, 1)),
                'C' => self.cuf(canonicalize_params_1(params, 1)),
                'D' => self.cub(canonicalize_params_1(params, 1)),
                'E' => self.cnl(canonicalize_params_1(params, 1)),
                'F' => self.cpl(canonicalize_params_1(params, 1)),
                'G' => self.cha(canonicalize_params_1(params, 1)),
                'H' | 'f' => {
                    self.cup(canonicalize_params_2(params, 1, 1));
                }
                'J' => self.ed(canonicalize_params_1(params, 0), unhandled),
                'K' => self.el(canonicalize_params_1(params, 0), unhandled),
                'L' => self.il(canonicalize_params_1(params, 1)),
                'M' => self.dl(canonicalize_params_1(params, 1)),
                'P' => self.dch(canonicalize_params_1(params, 1)),
                'S' => self.su(canonicalize_params_1(params, 1)),
                'T' => self.sd(canonicalize_params_1(params, 1)),
                'X' => self.ech(canonicalize_params_1(params, 1)),
                '`' => self.hpa(canonicalize_params_1(params, 1)),
                'b' => self.rep(canonicalize_params_1(params, 1)),
                'd' => self.vpa(canonicalize_params_1(params, 1)),
                'g' => self.tbc(canonicalize_params_1(params, 0)),
                'h' => self.sm(params, unhandled),
                'l' => self.rm(params, unhandled),
                'm' => self.sgr(params, unhandled),
                'r' => self.decstbm(canonicalize_params_decstbm(
                    params,
                    self.screen.grid().size(),
                )),
                't' => {
                    let mut params_iter = params.iter();
                    let op =
                        params_iter.next().and_then(|x| x.first().copied());
                    if op == Some(8) {
                        let (screen_rows, screen_cols) = self.screen.size();
                        let rows = params_iter
                            .next()
                            .and_then(|p| p.first().copied())
                            .unwrap_or_else(|| screen_rows.get());
                        let cols = params_iter
                            .next()
                            .and_then(|p| p.first().copied())
                            .unwrap_or_else(|| screen_cols.get());
                        self.callbacks.resize(&mut self.screen, (rows, cols));
                    } else {
                        self.callbacks.unhandled_csi(
                            &mut self.screen,
                            None,
                            None,
                            &params.iter().collect::<Vec<_>>(),
                            c,
                        );
                    }
                }
                _ => {
                    self.callbacks.unhandled_csi(
                        &mut self.screen,
                        None,
                        None,
                        &params.iter().collect::<Vec<_>>(),
                        c,
                    );
                }
            },
            Some(b' ') => match c {
                'q' => {
                    self.decscusr(
                        canonicalize_params_1(params, 0),
                        unhandled,
                    );
                }
                _ => {
                    self.callbacks.unhandled_csi(
                        &mut self.screen,
                        None,
                        None,
                        &params.iter().collect::<Vec<_>>(),
                        c,
                    );
                }
            },
            Some(b'?') => {
                match c {
                    'J' => self
                        .decsed(canonicalize_params_1(params, 0), unhandled),
                    'K' => self
                        .decsel(canonicalize_params_1(params, 0), unhandled),
                    'h' => self.decset(params, unhandled),
                    'l' => self.decrst(params, unhandled),
                    _ => {
                        self.callbacks.unhandled_csi(
                            &mut self.screen,
                            Some(b'?'),
                            intermediates.get(1).copied(),
                            &params.iter().collect::<Vec<_>>(),
                            c,
                        );
                    }
                }
            }
            Some(i) => {
                self.callbacks.unhandled_csi(
                    &mut self.screen,
                    Some(*i),
                    intermediates.get(1).copied(),
                    &params.iter().collect::<Vec<_>>(),
                    c,
                );
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bel_terminated: bool) {
        match params {
            [b"8", options, uri @ ..] if !uri.is_empty() => {
                self.screen.set_hyperlink(options, &uri.join(&b';'));
            }
            [b"133", arg, rest @ ..] => {
                match arg.first() {
                    Some(b'A' | b'N') => self.screen.mark_cursor_row(crate::ROW_START_PROMPT),
                    Some(b'P') if !rest.contains(&&b"k=s"[..]) => self.screen.mark_cursor_row(crate::ROW_START_PROMPT),
                    Some(b'P') => self.screen.mark_cursor_row(crate::ROW_SECOND_PROMPT),
                    Some(b'B' | b'I') => self.screen.mark_cursor_row(crate::ROW_START_COMMAND),
                    Some(b'C') => self.screen.mark_cursor_row(crate::ROW_START_OUTPUT),
                    Some(b'D') => self.screen.mark_cursor_row(crate::ROW_END_OUTPUT),
                    _ => {}
                }
                self.callbacks.unhandled_osc_terminated(&mut self.screen, params, bel_terminated);
            }
            [b"0", s] => {
                self.callbacks.set_window_icon_name(&mut self.screen, s);
                self.callbacks.set_window_title(&mut self.screen, s);
            }
            [b"1", s] => {
                self.callbacks.set_window_icon_name(&mut self.screen, s);
            }
            [b"2", s] => {
                self.callbacks.set_window_title(&mut self.screen, s);
            }
            [b"52", ty, data] => {
                // tmux keeps the allowed selector characters and drops the rest
                match (
                    ty.iter().all(|c| !matches!(c, 0..=0x1f | 0x7f)),
                    *data,
                ) {
                    (true, b"?") => {
                        self.callbacks
                            .clipboard_query(&mut self.screen, ty, bel_terminated);
                    }
                    (true, data)
                        if data.iter().all(|c| BASE64.contains(c)) =>
                    {
                        self.callbacks.copy_to_clipboard(
                            &mut self.screen,
                            ty,
                            data,
                        );
                    }
                    _ => {
                        self.callbacks
                            .unhandled_osc_terminated(&mut self.screen, params, bel_terminated);
                    }
                }
            }
            _ => {
                self.callbacks.unhandled_osc_terminated(&mut self.screen, params, bel_terminated);
            }
        }
    }
}

fn canonicalize_params_1(params: &vte::Params, default: u16) -> u16 {
    let first = params.iter().next().map_or(0, |x| *x.first().unwrap_or(&0));
    if first == 0 { default } else { first }
}

fn canonicalize_params_2(
    params: &vte::Params,
    default1: u16,
    default2: u16,
) -> (u16, u16) {
    let mut iter = params.iter();
    let first = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let first = if first == 0 { default1 } else { first };

    let second = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let second = if second == 0 { default2 } else { second };

    (first, second)
}

fn canonicalize_params_decstbm(
    params: &vte::Params,
    size: crate::grid::Size,
) -> (u16, u16) {
    let mut iter = params.iter();
    let top = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let top = if top == 0 { 1 } else { top };

    let bottom = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let bottom = if bottom == 0 { size.rows() } else { bottom };

    (top, bottom)
}
