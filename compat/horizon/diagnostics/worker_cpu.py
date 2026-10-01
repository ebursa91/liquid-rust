#!/usr/bin/env python3
"""Linux worker CPU diagnostic; requests cross framed pipes, not HTTP RPS."""
import argparse, asyncio, hashlib, json, os, random, statistics, subprocess, sys, time
from pathlib import Path
EXPECTED={'html':{'bytes':435304,'sha256':'d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990'},'css':{'bytes':279112,'sha256':'67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034'}}

def fingerprint(path):
 b=Path(path).read_bytes(); return {'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}

def cpu(pid):
 fields=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
 ticks=os.sysconf('SC_CLK_TCK')
 return {'user_seconds':int(fields[11])/ticks,'system_seconds':int(fields[12])/ticks}

async def run(name, command, args, env, batch, order):
 log=args.output_dir/f'batch-{batch:02d}-{name}.log'
 with log.open('wb') as stderr:
  p=await asyncio.create_subprocess_exec('taskset','-c',str(args.worker_cpu),*command,stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=stderr,start_new_session=True,env=env,limit=8192)
  try:
   ready=json.loads(await asyncio.wait_for(p.stdout.readline(),300))
   assert ready['action']=='ready' and ready['warmup']==args.warmup and ready['response_cache'] is False and ready['correctness_verified'] is True
   assert ready['scope']=='page' and ready['page']=='index' and ready['theme_sha']=='5acd1b6b66c02f61d3216e3adace5dd9e0404fc9'
   assert ready['fixture_sha256']=='867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98'
   worker_affinity=sorted(os.sched_getaffinity(p.pid)); assert worker_affinity==[args.worker_cpu]
   assert {k:ready[k] for k in EXPECTED}==EXPECTED
   if name=='ruby_yjit':
    assert ready['engine']=='ruby' and ready['yjit_enabled'] is True and ready['ruby_version']=='4.0.7' and ready['execution_mode']=='direct'
   else: assert ready['engine']=='liquid-rust' and ready['release'] is True and ready['build_profile']=='release'
   before=cpu(p.pid); start=time.perf_counter(); samples=[]
   for identifier in range(args.iterations):
    t=time.perf_counter(); p.stdin.write(json.dumps({'action':'render','request_id':identifier}).encode()+b'\n'); await p.stdin.drain()
    header=json.loads(await asyncio.wait_for(p.stdout.readline(),30))
    assert header['request_id']==identifier and header['error'] is None
    for key in EXPECTED:
     assert header[key+'_bytes']==EXPECTED[key]['bytes']
     body=await asyncio.wait_for(p.stdout.readexactly(header[key+'_bytes']),30)
     assert hashlib.sha256(body).hexdigest()==EXPECTED[key]['sha256']
    samples.append((time.perf_counter()-t)*1000)
   after=cpu(p.pid); elapsed=time.perf_counter()-start
   delta={k:after[k]-before[k] for k in before}
   result={'variant':name,'batch':batch,'order':order,'iterations':args.iterations,'worker_affinity':worker_affinity,'samples_pipe_wall_ms':samples,'median_pipe_wall_ms':statistics.median(samples),'phase_wall_seconds':elapsed,'worker_cpu_seconds':delta,'worker_cpu_ms_per_request':sum(delta.values())*1000/args.iterations,'load_after':list(os.getloadavg()),'correctness_verified':True,'ready':{k:ready.get(k) for k in ('engine','runtime','ruby_version','yjit_enabled','warmup','response_cache','html','css')}}
   p.stdin.close(); await asyncio.wait_for(p.wait(),30); assert p.returncode==0
   print(name,'batch',batch,'CPUms',result['worker_cpu_ms_per_request'],'pipeWallMedian',result['median_pipe_wall_ms'],flush=True)
   return result
  finally:
   try: os.killpg(p.pid,9)
   except ProcessLookupError: pass
   await p.wait()

async def main(args):
 args.output_dir.mkdir(parents=True,exist_ok=False)
 env=os.environ.copy()
 for k in ('RUBYLIB','RUBYOPT','RUBY_YJIT_ENABLE','GEM_HOME','GEM_PATH','BUNDLE_PATH','HORIZON_DATA_TOKEN','HORIZON_STORE_TOKEN'):env.pop(k,None)
 env.update(BUNDLE_GEMFILE=str(args.ruby_root/'Gemfile'),GEM_HOME=str(args.ruby_gem_home),GEM_PATH=str(args.ruby_gem_home))
 shared=['--theme-root',str(args.theme_root),'--fixture',str(args.fixture),'--scope','page','--page','index','--warmup',str(args.warmup),'--serve-stdio']
 commands={'rust_baseline':[str(args.baseline),*shared],'rust_regex_cache':[str(args.candidate),*shared], 'ruby_yjit':[str(args.ruby),'--yjit','-rbundler/setup',str(args.ruby_root/'benchmark/serve_worker.rb'),'--liquid-root',str(args.liquid_root),'--theme-root',str(args.theme_root),'--fixture',str(args.fixture),'--warmup',str(args.warmup),'--mode','direct']}
 inputs={k:fingerprint(v) for k,v in [('fixture',args.fixture),('baseline',args.baseline),('candidate',args.candidate),('ruby',args.ruby)]}
 sys.path.insert(0,str(args.rust_root/'compat/horizon')); import benchmark as common
 source_roots={'rust':args.rust_root,'candidate':args.candidate_root,'ruby':args.ruby_root,'theme':args.theme_root,'liquid':args.liquid_root}
 sources={k:common.repository(root.resolve()) for k,root in source_roots.items()}
 assert all(not sources[k]['dirty'] for k in ('rust','ruby','theme','liquid'))
 assert sources['rust']['head']==sources['candidate']['head']=='63df2051d8501a80b764681f7c797ea0907ba718'
 assert sources['theme']['head']=='5acd1b6b66c02f61d3216e3adace5dd9e0404fc9' and sources['liquid']['head']=='4e39ae4cc3da73921923c0669e0fc84a66b2f696'
 assert fingerprint(args.candidate_root/'examples/horizon.rs')['sha256']==args.candidate_source_sha
 assert subprocess.check_output(['git','-C',str(args.candidate_root),'diff','--name-only','HEAD'],text=True).strip()=='examples/horizon.rs'
 rng=random.Random(args.seed); runs=[]; started=time.time(); load_start=list(os.getloadavg())
 for batch in range(args.batches):
  names=list(commands); rng.shuffle(names)
  for order,name in enumerate(names):runs.append(await run(name,commands[name],args,env,batch,order))
 assert inputs=={k:fingerprint(v) for k,v in [('fixture',args.fixture),('baseline',args.baseline),('candidate',args.candidate),('ruby',args.ruby)]}
 assert sources=={k:common.repository(root.resolve()) for k,root in source_roots.items()}
 summary={}
 for name in commands:
  selected=[x for x in runs if x['variant']==name]; vals=[x['worker_cpu_ms_per_request'] for x in selected]
  summary[name]={'batch_worker_cpu_ms_per_request':vals,'median_worker_cpu_ms_per_request':statistics.median(vals),'min_worker_cpu_ms_per_request':min(vals),'max_worker_cpu_ms_per_request':max(vals),'batch_median_pipe_wall_ms':[x['median_pipe_wall_ms'] for x in selected]}
 result={'schema_version':1,'correctness_verified':True,'started_unix':started,'completed_unix':time.time(),'sources':sources,'inputs':inputs,'configuration':{k:getattr(args,k) for k in ('batches','iterations','warmup','worker_cpu','seed')},'controller_affinity':sorted(os.sched_getaffinity(0)),'cpu_tick_seconds':1/os.sysconf('SC_CLK_TCK'),'artifacts':EXPECTED,'load_start':load_start,'load_end':list(os.getloadavg()),'summary':summary,'runs':runs,'limits':['Diagnostic worker CPU experiment, not HTTP RPS or render-only wall time.','Linux /proc/pid/stat user+system CPU delta from READY after excluded warmup to final verified response; 10ms tick resolution; CPU average includes render, JSON framing and pipe writes, excludes startup/warmup and parent hashing CPU.','Per-response pipe wall includes worker queue, rendering, framing, pipe transfer and parent SHA verification.','Serial randomized variant order each batch; affinity not host reservation; shared host load/frequency uncontrolled.','Candidate uncommitted patch measured separately from clean baseline; record its source and patch digests in final publication.']}
 (args.output_dir/'cpu.json').write_text(json.dumps(result,indent=2)+'\n')
 print(json.dumps(summary),flush=True)

if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__)
 for k in ('baseline','candidate','candidate-root','ruby','ruby-root','ruby-gem-home','rust-root','liquid-root','theme-root','fixture','output-dir'):p.add_argument('--'+k,type=Path,required=True)
 p.add_argument('--candidate-source-sha',required=True)
 p.add_argument('--batches',type=int,default=3);p.add_argument('--iterations',type=int,default=100);p.add_argument('--warmup',type=int,default=50);p.add_argument('--worker-cpu',type=int,default=2);p.add_argument('--seed',type=int,default=20261001)
 args=p.parse_args(); assert args.batches>0 and args.iterations>0 and args.warmup>=1
 asyncio.run(main(args))
