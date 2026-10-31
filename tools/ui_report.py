"""Compare recorded terminal cells and build a self-contained visual UI report.

python tools/ui_report.py target/ui-comparison
Produces report.html, summary.json and selected PNG comparisons. It never alters
the application or treats recorder completion as a compatibility pass.
"""
import argparse
import json
import re
from pathlib import Path
import unicodedata


PALETTE = ['#000000', '#800000', '#008000', '#808000', '#000080', '#800080', '#008080', '#c0c0c0',
           '#808080', '#ff0000', '#00ff00', '#ffff00', '#0000ff', '#ff00ff', '#00ffff', '#ffffff']


def color(value, foreground):
    if value == 'default':
        return '#c0c0c0' if foreground else '#000000'
    if value.startswith('#'):
        return value
    n = int(value.split(':')[1])
    if n < 16:
        return PALETTE[n]
    if n < 232:
        n -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return '#%02x%02x%02x' % (levels[n // 36], levels[(n // 6) % 6], levels[n % 6])
    return '#%02x%02x%02x' % ((8 + (n - 232) * 10,) * 3)


def visible(cell):
    text, fg, bg, attrs = cell
    fg, bg = color(fg, True), color(bg, False)
    if attrs & 16:
        fg, bg = bg, fg
    # Foreground/bold on an un-underlined blank does not affect its appearance.
    attrs &= 15
    if text in ('', ' ') and not attrs & 8:
        fg, attrs = '', 0
    return unicodedata.normalize('NFC', text), fg, bg, attrs


def compare(reference, candidate):
    positions, text, style = [], 0, 0
    for y, (row_a, row_b) in enumerate(zip(reference['cells'], candidate['cells'])):
        for x, (a, b) in enumerate(zip(row_a, row_b)):
            va, vb = visible(a), visible(b)
            text += va[0] != vb[0]
            style += va[1:] != vb[1:]
            if va != vb:
                positions.append([x, y])
    cursor_a, cursor_b = reference['cursor'], candidate['cursor']
    cursor = cursor_a['visible'] != cursor_b['visible'] or (
        cursor_a['visible'] and cursor_b['visible'] and cursor_a != cursor_b)
    geometry = (reference['width'], reference['height']) != (candidate['width'], candidate['height'])
    rows_a = [''.join(visible(c)[0] for c in row) for row in reference['cells']]
    rows_b = [''.join(visible(c)[0] for c in row) for row in candidate['cells']]
    # Pane IDs are runtime data. Keep the raw difference visible in the report;
    # permit only this token in this known default-menu capture during --check.
    dynamic_id_only = (reference['name'] == '22-default-pane-menu' and bool(positions)
                       and not style and not cursor and not geometry and rows_a[1:] == rows_b[1:]
                       and re.sub(r'%\d+', '%ID', rows_a[0]) == re.sub(r'%\d+', '%ID', rows_b[0]))
    return dict(name=reference['name'], text_cells=text, style_cells=style,
                dynamic_id_only=dynamic_id_only,
                visible_cells=len(positions), cursor_diff=cursor, geometry_diff=geometry,
                modes_diff=reference['modes'] != candidate['modes'],
                state_diff=reference['state'] != candidate['state'],
                menu_result_diff=reference['menu_result'] != candidate['menu_result'],
                copy_result_diff=reference.get('copy_result') != candidate.get('copy_result'),
                cli_wait_diff=reference['pending_cli'] != candidate['pending_cli'],
                errors=bool(reference['errors'] or candidate['errors']),
                screen_matches=not positions and not cursor and not geometry,
                difference_positions=positions)


def write_png(path, reference, candidate, title):
    from PIL import Image, ImageDraw, ImageFont
    fonts = Path('C:/Windows/Fonts')
    regular = ImageFont.truetype(str(fonts / 'consola.ttf'), 16)
    bold = ImageFont.truetype(str(fonts / 'consolab.ttf'), 16)
    cjk = ImageFont.truetype(str(fonts / 'malgun.ttf'), 16)
    cell_w, cell_h, margin, header = 11, 22, 16, 48
    w = reference['width'] * cell_w
    h = reference['height'] * cell_h
    image = Image.new('RGB', (w * 2 + margin * 3, h + header + margin), '#202733')
    draw = ImageDraw.Draw(image)
    diff = compare(reference, candidate)
    for side, (label, snapshot) in enumerate((('tmux 3.6a', reference), ('pwsh-tmux', candidate))):
        ox = margin + side * (w + margin)
        draw.text((ox, 8), label + '  |  ' + title, font=regular, fill='#ffffff')
        # Paint all backgrounds before glyphs, preserving wide/combining glyphs.
        for y, row in enumerate(snapshot['cells']):
            for x, (_, fg, bg, attrs) in enumerate(row):
                fg, bg = color(fg, True), color(bg, False)
                if attrs & 16:
                    fg, bg = bg, fg
                xx, yy = ox + x * cell_w, header + y * cell_h
                draw.rectangle((xx, yy, xx + cell_w - 1, yy + cell_h - 1), fill=bg)
        for y, row in enumerate(snapshot['cells']):
            for x, cell in enumerate(row):
                text, fg, bg, attrs = cell
                fg, bg = color(fg, True), color(bg, False)
                if attrs & 16:
                    fg, bg = bg, fg
                xx, yy = ox + x * cell_w, header + y * cell_h
                if attrs & 2:
                    fg = '#%02x%02x%02x' % tuple(int(int(fg[n:n+2],16)*.55 + int(bg[n:n+2],16)*.45) for n in (1,3,5))
                if text:
                    font = cjk if any(ord(c) > 0x2e7f for c in text) else bold if attrs & 1 else regular
                    draw.text((xx, yy), text, font=font, fill=fg)
                    if attrs & 8:
                        draw.line((xx, yy + cell_h - 3, xx + cell_w - 1, yy + cell_h - 3), fill=fg)
        cursor = snapshot['cursor']
        if cursor['visible'] and cursor['x'] < snapshot['width'] and cursor['y'] < snapshot['height']:
            xx, yy = ox + cursor['x'] * cell_w, header + cursor['y'] * cell_h
            draw.rectangle((xx, yy, xx + cell_w - 1, yy + cell_h - 1), outline='#ffffff')
        if side:
            for x, y in diff['difference_positions']:
                xx, yy = ox + x * cell_w, header + y * cell_h
                draw.line((xx, yy + cell_h - 1, xx + cell_w - 1, yy + cell_h - 1), fill='#ff4669')
    image.save(path)


PAGE = r'''<!doctype html><html lang="ko"><meta charset="utf-8"><title>tmux UI/UX 비교</title>
<style>
body{margin:0;padding:24px;background:#101722;color:#e4eaf4;font:15px system-ui,sans-serif}h1{margin:0 0 12px}p{max-width:1100px;line-height:1.7;color:#b9c6dc}
header{position:sticky;top:0;background:#101722;padding:12px 0;z-index:2}select,button{padding:9px;background:#23324a;color:white;border:1px solid #4d6285;border-radius:5px;margin-right:8px}select#cases{min-width:340px}
.screens{display:grid;grid-template-columns:1fr 1fr;gap:16px}.panel{min-width:0;background:#1d293c;border-radius:8px;padding:12px}.panel h2{font-size:16px;margin:0 0 10px}canvas{width:100%;height:auto;background:#000;image-rendering:auto}pre{white-space:pre-wrap;color:#b5c9e5;font-size:12px}#stats{margin:12px 0;color:#ffbd69}.note{font-size:12px}.good{color:#7be3ab}@media(max-width:1050px){.screens{grid-template-columns:1fr}}
</style>
<h1>tmux 3.6a / 프로젝트 UI·UX 비교</h1>
<p>실제 PTY 출력의 글자·색상·속성·커서와 조작 결과를 기록했습니다. 아래 화면은 셀 데이터 재구성이며 운영체제의 실제 폰트·DPI 픽셀 캡처가 아닙니다. 시계만 고정했고 동일 콘텐츠를 사용했습니다. 오른쪽 빨간 밑줄은 다른 셀입니다.</p>
<header><select id="platform"></select><button id="prev">←</button><select id="cases"></select><button id="next">→</button><label><input id="diff" type="checkbox" checked>차이 표시</label><div id="stats"></div></header>
<div class="screens"><section class="panel"><h2>원본 tmux 3.6a</h2><canvas id="reference"></canvas><pre id="refmeta"></pre></section><section class="panel"><h2>프로젝트 pwsh-tmux</h2><canvas id="candidate"></canvas><pre id="candmeta"></pre></section></div>
<p class="note">완전히 일치한 화면도 포함됩니다. 숨겨진 커서 좌표와 빈칸의 전경색 차이는 가시적 차이 집계에서 제외합니다. Unicode는 NFC로 비교합니다. 기본 트리의 동적 ID·이력 수치와 시계 값에는 환경 요인도 포함될 수 있습니다. CLI 오류가 있으면 그 사례는 일치/불일치 집계에서 제외합니다.</p>
<script>const DATA=__DATA__;
const el=id=>document.getElementById(id),palette=__PALETTE__;
function color(c,fg){if(c==='default')return fg?'#c0c0c0':'#000000';if(c[0]==='#')return c;let n=+c.split(':')[1];if(n<16)return palette[n];if(n<232){n-=16;let v=[0,95,135,175,215,255];return '#'+[v[Math.floor(n/36)],v[Math.floor(n/6)%6],v[n%6]].map(x=>x.toString(16).padStart(2,'0')).join('')}return '#'+(8+(n-232)*10).toString(16).padStart(2,'0').repeat(3)}
function paint(id,snap,diffs){let cv=el(id),ctx=cv.getContext('2d'),cw=12,ch=23;cv.width=snap.width*cw;cv.height=snap.height*ch;ctx.textBaseline='top';for(let y=0;y<snap.height;y++)for(let x=0;x<snap.width;x++){let[,f,b,a]=snap.cells[y][x],fg=color(f,1),bg=color(b,0);if(a&16)[fg,bg]=[bg,fg];ctx.fillStyle=bg;ctx.fillRect(x*cw,y*ch,cw,ch)}for(let y=0;y<snap.height;y++)for(let x=0;x<snap.width;x++){let[t,f,b,a]=snap.cells[y][x],fg=color(f,1),bg=color(b,0);if(a&16)[fg,bg]=[bg,fg];ctx.globalAlpha=a&2?.55:1;ctx.fillStyle=fg;ctx.font=`${a&4?'italic ':''}${a&1?'bold ':''}18px Consolas,monospace`;if(t)ctx.fillText(t,x*cw,y*ch+1);if(a&8)ctx.fillRect(x*cw,y*ch+ch-3,cw,1)}ctx.globalAlpha=1;let c=snap.cursor;if(c.visible&&c.x<snap.width){ctx.strokeStyle='#ffffff';ctx.strokeRect(c.x*cw+.5,c.y*ch+.5,cw-1,ch-1)}if(diffs&&el('diff').checked){ctx.fillStyle='#ff4669';for(let[x,y]of diffs)ctx.fillRect(x*cw,y*ch+ch-2,cw,2)}}
function render(){let set=DATA[el('platform').value],i=+el('cases').value,r=set.reference.cases[i],c=set.candidate.cases[i],d=set.results[i];paint('reference',r);paint('candidate',c,d.difference_positions);el('stats').textContent=`${d.screen_matches?'画面一致 / 화면 일치':'화면 차이'} · 글자 ${d.text_cells}셀 · 스타일 ${d.style_cells}셀 · 커서 ${d.cursor_diff?'차이':'일치'} · 상태 ${d.state_diff?'차이':'일치'} · CLI 대기 ${d.cli_wait_diff?'차이':'일치'}${d.errors?' · 명령 오류 포함':''}`;for(let[id,s]of[['refmeta',r],['candmeta',c]])el(id).textContent=JSON.stringify({state:s.state,menu_result:s.menu_result,pending_cli:s.pending_cli,copy_result:s.copy_result,cursor:s.cursor,modes:s.modes,errors:s.errors},null,2)}
function platform(){let s=DATA[el('platform').value];el('cases').innerHTML=s.results.map((r,i)=>`<option value="${i}">${r.screen_matches?'✓':'Δ'} ${r.name}</option>`).join('');render()}
el('platform').innerHTML=Object.keys(DATA).map(k=>`<option>${k}</option>`).join('');el('platform').onchange=platform;el('cases').onchange=render;el('diff').onchange=render;el('prev').onclick=()=>{el('cases').selectedIndex=Math.max(0,el('cases').selectedIndex-1);render()};el('next').onclick=()=>{el('cases').selectedIndex=Math.min(el('cases').options.length-1,el('cases').selectedIndex+1);render()};platform();</script></html>'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--check', action='store_true', help='fail on unexpected screen, state, input, command or copy differences')
    parser.add_argument('--expected-cases', type=int, default=52, help='required case count for each platform during --check')
    args = parser.parse_args()
    data, summaries = {}, {}
    for directory in sorted(args.directory.iterdir()):
        if not directory.is_dir() or not (directory / 'reference.json').exists():
            continue
        reference = json.loads((directory / 'reference.json').read_text(encoding='utf-8'))
        candidate = json.loads((directory / 'candidate.json').read_text(encoding='utf-8'))
        assert [s['name'] for s in reference['cases']] == [s['name'] for s in candidate['cases']]
        results = [compare(a, b) for a, b in zip(reference['cases'], candidate['cases'])]
        data[directory.name] = dict(reference=reference, candidate=candidate, results=results)
        valid = [r for r in results if not r['errors']]
        summaries[directory.name] = dict(captured=len(results), command_error_cases=len(results)-len(valid),
                                         screen_matches=sum(r['screen_matches'] for r in valid),
                                         screen_differences=sum(not r['screen_matches'] for r in valid),
                                         dynamic_id_only=sum(r['dynamic_id_only'] for r in valid),
                                         state_differences=sum(r['state_diff'] for r in valid),
                                         copy_result_differences=sum(r['copy_result_diff'] for r in valid),
                                         terminal_mode_differences=sum(r['modes_diff'] for r in valid),
                                         cli_wait_differences=sum(r['cli_wait_diff'] for r in valid))
        (directory / 'differences.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        for a, b in zip(reference['cases'], candidate['cases']):
            if a['name'].startswith(('01-', '03-', '19-', '23-', '25-', '33-', '39-')):
                write_png(directory / (a['name'] + '.png'), a, b, a['name'])
    page = PAGE.replace('__DATA__', json.dumps(data, ensure_ascii=False).replace('<', '\u003c')).replace('__PALETTE__', json.dumps(PALETTE))
    (args.directory / 'report.html').write_text(page, encoding='utf-8')
    (args.directory / 'summary.json').write_text(json.dumps(summaries, indent=2), encoding='utf-8')
    print(json.dumps(summaries, indent=2))
    if args.check:
        failures = [(platform, result['name']) for platform, group in data.items() for result in group['results']
                    if (not result['screen_matches'] and not result['dynamic_id_only'])
                    or any(result[key] for key in ('errors', 'modes_diff', 'state_diff', 'menu_result_diff', 'copy_result_diff', 'cli_wait_diff'))]
        incomplete = [platform for platform, group in data.items() if len(group['results']) != args.expected_cases]
        if not data or failures or incomplete:
            raise SystemExit('Unexpected UI differences: ' + repr(failures) + '; incomplete captures: ' + repr(incomplete))


if __name__ == '__main__':
    main()
