#!/usr/bin/env python3
"""summarize.py [--ref IMPL] [--max-throttled-ms MS] RESULTS.json [MORE.json ...]
Prints a markdown table (median [min-max] (n) per implementation and the ratio
to the reference), writes summary.json next to the first input. Raw samples of
all inputs are merged per implementation (repetitions are concatenated).
  --ref IMPL             ratio denominator (default: tmux if present, else the first implementation)
  --max-throttled-ms MS  leave out repetitions whose test recorded more cgroup CPU
                         throttling than MS (tests that record throttled_ms: throughput,
                         latency, heavy flood); the table shows how many were left out
"""
import json, os, statistics, sys

M = [  # (section, label, path, unit, scale)
    ('idle', '1 session/1 pane: server CPU per 30 s', 'idle_a/server/cpu', 'ms', 1000),
    ('idle', '1 session/1 pane: server wakeups', 'idle_a/server/wakeups_per_s', '/s', 1),
    ('idle', '1 session/1 pane: server RSS', 'idle_a/mem/VmRSS', 'MiB', 1 / 1024),
    ('idle', '1 session/1 pane: server anon RSS', 'idle_a/mem/RssAnon', 'MiB', 1 / 1024),
    ('idle', '1 session/1 pane: server PSS', 'idle_a/mem/Pss', 'MiB', 1 / 1024),
    ('idle', '80 panes: server CPU per 30 s', 'idle_b/server/cpu', 'ms', 1000),
    ('idle', '80 panes: server wakeups', 'idle_b/server/wakeups_per_s', '/s', 1),
    ('idle', '80 panes: server RSS', 'idle_b/mem/VmRSS', 'MiB', 1 / 1024),
    ('idle', '80 panes: server threads', 'idle_b/server_threads', '', 1),
    ('idle', 'attached+clock+copy+status 1s: server CPU per 30 s', 'idle_c/server/cpu', 'ms', 1000),
    ('idle', 'attached+clock+copy+status 1s: server wakeups', 'idle_c/server/wakeups_per_s', '/s', 1),
    ('idle', 'attached+clock+copy+status 1s: client CPU per 30 s', 'idle_c/client/cpu', 'ms', 1000),
    ('idle', 'attached+clock+copy+status 1s: client wakeups', 'idle_c/client/wakeups_per_s', '/s', 1),
    ('idle', 'attached+clock+copy+status 1s: bytes to terminal per 30 s', 'idle_c/client_bytes', 'KiB', 1 / 1024),
    ('idle', 'attached: server RSS', 'idle_c/mem/VmRSS', 'MiB', 1 / 1024),
    ('idle', 'attached: client RSS', 'idle_c/client_mem/VmRSS', 'MiB', 1 / 1024),
    ('memory', 'history 50000 x seq lines: server RSS after', 'memory/hist_seq/after/VmRSS', 'MiB', 1 / 1024),
    ('memory', 'history 50000 x seq lines: RSS growth', 'memory/hist_seq/delta_rss_kb', 'MiB', 1 / 1024),
    ('memory', 'history 50000 x ~80-col text: server RSS after', 'memory/hist_text/after/VmRSS', 'MiB', 1 / 1024),
    ('memory', 'history 50000 x ~80-col text: RSS growth', 'memory/hist_text/delta_rss_kb', 'MiB', 1 / 1024),
    ('memory', 'fill 200000 seq lines (no client): wall', 'memory/hist_seq/fill_secs', 's', 1),
    ('memory', '100 panes: server RSS', 'memory/panes100/hundred/VmRSS', 'MiB', 1 / 1024),
    ('memory', 'per-pane RSS overhead', 'memory/panes100/per_pane_rss_kb', 'KiB', 1),
    ('memory', 'per-pane anon overhead', 'memory/panes100/per_pane_anon_kb', 'KiB', 1),
    ('throughput', '50 MB text, no client: wall', 'throughput/text50_noclient/secs', 's', 1),
    ('throughput', '50 MB text, no client: server CPU', 'throughput/text50_noclient/server/cpu', 's', 1),
    ('throughput', '50 MB text, client 120x40: wall', 'throughput/text50_client/secs', 's', 1),
    ('throughput', '50 MB text, client 120x40: server CPU', 'throughput/text50_client/server/cpu', 's', 1),
    ('throughput', '50 MB text, client 120x40: client CPU', 'throughput/text50_client/client/cpu', 's', 1),
    ('throughput', '50 MB text, client: bytes to terminal', 'throughput/text50_client/client_bytes', 'MiB', 1 / 1048576),
    ('throughput', '50 MB colour, no client: wall', 'throughput/colour50_noclient/secs', 's', 1),
    ('throughput', '50 MB colour, no client: server CPU', 'throughput/colour50_noclient/server/cpu', 's', 1),
    ('throughput', '50 MB colour, client 120x40: wall', 'throughput/colour50_client/secs', 's', 1),
    ('throughput', '50 MB colour, client 120x40: server CPU', 'throughput/colour50_client/server/cpu', 's', 1),
    ('throughput', '50 MB colour, client 120x40: client CPU', 'throughput/colour50_client/client/cpu', 's', 1),
    ('throughput', '50 MB colour, client: bytes to terminal', 'throughput/colour50_client/client_bytes', 'MiB', 1 / 1048576),
    ('latency', 'key echo p50 (quiet)', 'latency/quiet/p50_ms', 'ms', 1),
    ('latency', 'key echo p99 (quiet)', 'latency/quiet/p99_ms', 'ms', 1),
    ('latency', 'key echo p50 (flood in other pane)', 'latency/flood/p50_ms', 'ms', 1),
    ('latency', 'key echo p99 (flood in other pane)', 'latency/flood/p99_ms', 'ms', 1),
    ('latency', 'flood run: server CPU over 500 keys', 'latency/flood/server/cpu', 's', 1),
    ('latency', 'flood run: server CPU %', 'latency/flood/server_cpu_pct', '%', 1),
    ('latency', 'flood run: client CPU over 500 keys', 'latency/flood/client/cpu', 's', 1),
    ('commands', "display -p '#{pane_id}' per call", 'commands/display_pane_id/mean_ms', 'ms', 1),
    ('commands', 'list-windows -a per call', 'commands/list_windows_a/mean_ms', 'ms', 1),
    ('commands', 'capture-pane -p -S -1000 per call', 'commands/capture_pane_1000/mean_ms', 'ms', 1),
    ('commands', 'send-keys per call', 'commands/send_keys/mean_ms', 'ms', 1),
    ('commands', 'new-window + kill-window per cycle', 'commands/new_kill_window_cycle/mean_ms', 'ms', 1),
    ('commands', 'server CPU for 1000 display', 'commands/display_pane_id/server/cpu', 's', 1),
    ('commands', 'server CPU for 1000 capture-pane', 'commands/capture_pane_1000/server/cpu', 's', 1),
    ('commands', 'server CPU for 300 new/kill cycles', 'commands/new_kill_window_cycle/server/cpu', 's', 1),
    ('commands', 'server start (new -d) p50', 'commands/server_start_new_d_ms/p50_ms', 'ms', 1),
    ('commands', 'attach until status line drawn p50', 'commands/attach_ms/p50_ms', 'ms', 1),
    ('heavy', 'copy-mode on 50000 history: command', 'heavy/copy_mode_enter/cmd_p50_ms', 'ms', 1),
    ('heavy', 'copy-mode on 50000 history: until drawn', 'heavy/copy_mode_enter/visible_p50_ms', 'ms', 1),
    ('heavy', 'search-backward (regex) hit near top', 'heavy/search_found_top/p50_ms', 'ms', 1),
    ('heavy', 'search-backward (regex) no match', 'heavy/search_notfound/p50_ms', 'ms', 1),
    ('heavy', 'search-backward-text hit near top', 'heavy/search_text_found_top/p50_ms', 'ms', 1),
    ('heavy', 'resize-window 200x60<->80x24 (50000 history) per cycle', 'heavy/resize_100_cycles/per_cycle_ms', 'ms', 1),
    ('heavy', 'resize x100: server CPU', 'heavy/resize_100_cycles/server/cpu', 's', 1),
    ('heavy', 'choose-tree 40 windows: until drawn', 'heavy/choose_tree_40/visible_p50_ms', 'ms', 1),
    ('heavy', '5 clients + 50 MB flood: wall', 'heavy/flood_5_clients/secs', 's', 1),
    ('heavy', '5 clients + 50 MB flood: server CPU', 'heavy/flood_5_clients/server/cpu', 's', 1),
    ('heavy', '5 clients + 50 MB flood: clients CPU', 'heavy/flood_5_clients/clients_cpu_s', 's', 1),
]


def get(rep, path):
    for p in path:
        if not isinstance(rep, dict) or p not in rep:
            return None
        rep = rep[p]
    return rep


def throttled(rep, path):
    """The throttled_ms of the innermost test dict on `path`, or 0."""
    t = 0
    for p in path:
        if not isinstance(rep, dict):
            break
        if isinstance(rep.get('throttled_ms'), (int, float)):
            t = rep['throttled_ms']
        rep = rep.get(p)
    return t


def vals(raw, impl, path, scale, max_thr=None):
    g, rest = path.split('/', 1)
    out, dropped = [], 0
    for rep in raw.get(impl, {}).get(g, []):
        v = get(rep, rest.split('/'))
        if isinstance(v, (int, float)):
            if max_thr is not None and throttled(rep, rest.split('/')) > max_thr:
                dropped += 1
                continue
            out.append(v * scale)
    return out, dropped


def fmt(x):
    if x is None:
        return '-'
    a = abs(x)
    return f'{x:.0f}' if a >= 100 else f'{x:.1f}' if a >= 10 else f'{x:.2f}' if a >= 1 else f'{x:.3f}'


def main():
    args = sys.argv[1:]
    ref = max_thr = None
    while args and args[0].startswith('--'):
        if args[0] == '--ref' and len(args) > 1:
            ref = args[1]; args = args[2:]
        elif args[0] == '--max-throttled-ms' and len(args) > 1:
            max_thr = float(args[1]); args = args[2:]
        else:
            sys.exit(__doc__.strip())
    if not args:
        sys.exit(__doc__.strip())
    raw = {}
    meta = []
    for p in args:
        d = json.load(open(p))
        meta.append(d['meta'])
        for impl, groups in d['raw'].items():
            for g, reps in groups.items():
                raw.setdefault(impl, {}).setdefault(g, []).extend(reps)
    impls = list(raw)
    if ref is None:
        ref = 'tmux' if 'tmux' in impls else impls[0]
    if ref not in impls:
        sys.exit(f'--ref {ref}: not in {impls}')
    rows = []
    for sec, label, path, unit, scale in M:
        row = {'section': sec, 'metric': label, 'unit': unit, 'path': path}
        for i in impls:
            v, dropped = vals(raw, i, path, scale, max_thr)
            row[i] = {'median': statistics.median(v) if v else None, 'min': min(v) if v else None,
                      'max': max(v) if v else None, 'n': len(v), 'dropped_throttled': dropped,
                      'samples': [round(x, 4) for x in v]}
        rows.append(row)
    others = [i for i in impls if i != ref]
    for row in rows:
        r = row[ref]['median']
        for o in others:
            c = row[o]['median']
            row[f'ratio_{o}'] = (c / r) if (r not in (None, 0) and c is not None) else None
            row[f'diff_{o}'] = (c - r) if (r is not None and c is not None) else None
    lines = ['| section | metric | unit | ' + ' | '.join(f'{i} median [min-max] (n)' for i in impls) + ' | ' +
             ' | '.join(f'{o}/{ref}' for o in others) + ' |',
             '|' + '---|' * (3 + len(impls) + len(others))]
    for row in rows:
        cells = []
        for i in impls:
            s = row[i]
            drop = f"{s['dropped_throttled']} throttled left out" if s['dropped_throttled'] else ''
            if s['median'] is None:
                cells.append('-' + (f' ({drop})' if drop else ''))
            else:
                cells.append(f"{fmt(s['median'])} [{fmt(s['min'])}-{fmt(s['max'])}] ({s['n']}" + (f', {drop}' if drop else '') + ')')
        rat = [('-' if row[f'ratio_{o}'] is None else f"{row[f'ratio_{o}']:.2f}x") for o in others]
        lines.append(f"| {row['section']} | {row['metric']} | {row['unit']} | " + ' | '.join(cells) + ' | ' + ' | '.join(rat) + ' |')
    print('\n'.join(lines))
    json.dump({'meta': meta, 'ref': ref, 'max_throttled_ms': max_thr, 'rows': rows},
              open(os.path.join(os.path.dirname(args[0]) or '.', 'summary.json'), 'w'), indent=1)


if __name__ == '__main__':
    main()
