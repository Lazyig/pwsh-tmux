"""Repeatable RSS benchmark: 209x50, 1/8 windows, idle/1900 history rows, five runs.
AI programs, shell workloads and the terminal emulator are excluded.
"""
import argparse,shlex,os,sys,json,time,subprocess,struct,threading,statistics,uuid,pathlib,hashlib
parser=argparse.ArgumentParser(description="Compare tmux server + attached-client RSS on Unix. Uses private temporary servers only.")
parser.add_argument('--reference',required=True,type=pathlib.Path)
parser.add_argument('--candidate',required=True,type=pathlib.Path)
parser.add_argument('--output',required=True,type=pathlib.Path)
args=parser.parse_args()
if os.name != 'posix': parser.error('Run inside Linux/WSL with Linux binaries, or another Unix with /proc RSS support')
import pty,fcntl,termios
BASE=args.output.resolve()
BASE.mkdir(parents=True,exist_ok=True)
BINS={'original':str(args.reference.resolve()),'project':str(args.candidate.resolve())}
PREFIX='memory'
(BASE/'workload.py').write_text("import sys\nfor i in range(int(sys.argv[1])): sys.stdout.write(('%08d '%i+'abcdefghijklmnopqrstuvwxyz0123456789'*10)[:int(sys.argv[2])]+'\\r\\n')\nsys.stdout.flush()\n")
ENV=dict(os.environ,TERM='xterm-256color',SHELL='/bin/sh',PWSH_TMUX_SHELL='/bin/sh',LC_ALL='C.UTF-8')
ENV.pop('TMUX',None);ENV.pop('PWSH_TMUX',None)
(BASE/'empty.conf').write_text('set-option -g history-limit 2000\nset-option -g default-shell /bin/sh\n')
def rss(pid):
    with open('/proc/%s/status'%pid) as f:
        for line in f:
            if line.startswith('VmRSS:'):return int(line.split()[1])*1024
    raise RuntimeError('No VmRSS for '+str(pid))
def measure(server,client):
    samples=[]
    for _ in range(7):
        samples.append({'server':rss(server),'client':rss(client)})
        time.sleep(.08)
    return {k:int(statistics.median(s[k] for s in samples)) for k in ['server','client']}
def one(kind,count,repeat):
    binary=BINS[kind];sock='memorybench-'+uuid.uuid4().hex
    def run(*args):
        r=subprocess.run([binary,'-L',sock,*args],env=ENV,stdout=subprocess.PIPE,stderr=subprocess.PIPE,universal_newlines=True,timeout=20)
        if r.returncode:raise RuntimeError(str(args)+': '+r.stderr+r.stdout)
        return r.stdout.strip()
    client=None;master=None;slave=None
    try:
        run('-f',str(BASE/'empty.conf'),'new-session','-d','-s','bench','-n','w0','-x','209','-y','50','/bin/sleep','600')
        for i in range(1,count):run('new-window','-d','-t','bench','-n','w%d'%i,'/bin/sleep','600')
        server=int(run('display-message','-p','-t','bench','#{pid}'))
        master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',51,209,0,0))
        client=subprocess.Popen([binary,'-L',sock,'attach-session','-t','bench'],stdin=slave,stdout=slave,stderr=slave,env=ENV,start_new_session=True)
        os.close(slave);slave=None
        def drain():
            try:
                while os.read(master,65536):pass
            except OSError:pass
        threading.Thread(target=drain,daemon=True).start()
        time.sleep(.45)
        panes=run('list-panes','-s','-t','bench','-F','#{pane_id}|#{pane_width}|#{pane_height}|#{history_size}').splitlines()
        assert len(panes)==count,(kind,panes)
        idle=measure(server,client.pid)
        for row in panes:
            p,w,h,_=row.split('|');assert (int(w),int(h))==(209,50),(kind,row)
            command='%s %s %d %d; exec /bin/sleep 600'%(shlex.quote(sys.executable),shlex.quote(str(BASE/'workload.py')),1900+int(h)-1,int(w)-5)
            run('respawn-pane','-k','-t',p,'/bin/sh','-c',command)
        deadline=time.time()+15
        while True:
            histories=run('list-panes','-s','-t','bench','-F','#{history_size}').splitlines()
            if len(histories)==count and all(int(h)==1900 for h in histories):break
            if time.time()>deadline:raise RuntimeError('History mismatch: '+kind+str(histories))
            time.sleep(.12)
        time.sleep(.4)
        full=measure(server,client.pid)
        return {'kind':kind,'windows':count,'repeat':repeat,'server_pid':server,'client_pid':client.pid,'idle':idle,'history':full,'history_rows_per_pane':1900,'pane_size':[209,50]}
    finally:
        try:run('kill-server')
        except Exception:pass
        if client:
            try:client.wait(timeout=3)
            except subprocess.TimeoutExpired:client.terminate();client.wait(timeout=3)
        if master is not None:os.close(master)
        if slave is not None:os.close(slave)
data={'platform':subprocess.check_output(['uname','-a'],universal_newlines=True).strip(),'metric':'Linux /proc/PID/status VmRSS; server and one attached client only; child workloads excluded','repeats':5,'history_limit':2000,'history_rows':1900,'columns':209,'rows':50,'binaries':{},'runs':[]}
for kind,binary in BINS.items():
    data['binaries'][kind]={'path':binary,'version':subprocess.check_output([binary,'-V'],env=ENV,universal_newlines=True).strip(),'sha256':hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest()}
for repeat in range(1,6):
    for count in [1,8]:
        # Alternate implementation order to reduce systematic time/order bias.
        for kind in (['original','project'] if repeat%2 else ['project','original']):
            r=one(kind,count,repeat);data['runs'].append(r)
            (BASE/(PREFIX+'-raw.json')).write_text(json.dumps(data,indent=2))
            print(json.dumps(r),flush=True)
summary=[]
for count in [1,8]:
    for phase in ['idle','history']:
        row={'windows':count,'phase':phase}
        for kind in BINS:
            runs=[r for r in data['runs'] if r['kind']==kind and r['windows']==count]
            totals=[r[phase]['server']+r[phase]['client'] for r in runs]
            row[kind]={'server_median':statistics.median(r[phase]['server'] for r in runs),'client_median':statistics.median(r[phase]['client'] for r in runs),'total_median':statistics.median(totals),'total_min':min(totals),'total_max':max(totals)}
        row['ratio']=row['project']['total_median']/row['original']['total_median']
        row['difference_bytes']=row['project']['total_median']-row['original']['total_median']
        summary.append(row)
data['summary']=summary
(BASE/(PREFIX+'-results.json')).write_text(json.dumps(data,indent=2))
print('SUMMARY '+json.dumps(summary),flush=True)
