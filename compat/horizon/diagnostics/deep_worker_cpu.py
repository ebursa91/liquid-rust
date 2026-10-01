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
    assert ready['liquid_sha']=='4e39ae4cc3da73921923c0669e0fc84a66b2f696' and Path(ready['liquid_source']).resolve().is_relative_to(args.liquid_root.resolve())
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
   result={'variant':name,'batch':batch,'order':order,'iterations':args.iterations,'worker_affinity':worker_affinity,'samples_pipe_wall_ms':samples,'median_pipe_wall_ms':statistics.median(samples),'phase_wall_seconds':elapsed,'worker_cpu_seconds':delta,'worker_cpu_ms_per_request':sum(delta.values())*1000/args.iterations,'load_after':list(os.getloadavg()),'correctness_verified':True,'ready':ready}
   p.stdin.close(); await asyncio.wait_for(p.wait(),30); assert p.returncode==0
   print(name,'batch',batch,'CPUms',result['worker_cpu_ms_per_request'],'pipeWallMedian',result['median_pipe_wall_ms'],flush=True)
   return result
  finally:
   try: os.killpg(p.pid,9)
   except ProcessLookupError: pass
   await p.wait()

async def main(args):
 args.output_dir.mkdir(parents=True,exist_ok=False)
 manifest=json.loads(args.manifest.read_text())
 base=manifest['base_head']; assert base=='a468a7ac18155f98fb9fe51881eafac50bae8105'
 tracked=subprocess.check_output(['git','-C',str(args.rust_root),'ls-tree','-r','--name-only',base],text=True).splitlines()
 expected={path:hashlib.sha256(subprocess.check_output(['git','-C',str(args.rust_root),'show',base+':'+path])).hexdigest() for path in tracked}
 sources={}
 def archive_state(variant):
  root=Path(variant['source_root']); values={path:fingerprint(root/path) for path in tracked}
  changed={path:value['sha256'] for path,value in values.items() if value['sha256']!=expected[path]}
  assert changed==variant['changed_source_sha256'],(variant['name'],changed)
  extras=[str(path.relative_to(root)) for path in root.rglob('*.rs') if str(path.relative_to(root)) not in tracked and 'target' not in path.parts]
  assert not extras,(variant['name'],extras)
  patch=variant.get('patch'); patch_fp=fingerprint(patch) if patch else None
  if patch: assert patch_fp['sha256']==variant['patch_sha256']
  binary=fingerprint(variant['binary']); assert binary['sha256']==variant['binary_sha256']
  return {'base_head':base,'source_files':values,'changed_source_sha256':changed,'patch':patch_fp,'binary':binary,'build_flags':variant.get('build_flags',{}),'build_provenance':fingerprint(variant['build_provenance']) if variant.get('build_provenance') else None}
 for v in manifest['variants']:sources[v['name']]=archive_state(v)
 sys.path.insert(0,str(args.rust_root/'compat/horizon')); import benchmark as common
 source_roots={'ruby':args.ruby_root,'theme':args.theme_root,'liquid':args.liquid_root}
 other={k:common.repository(root.resolve()) for k,root in source_roots.items()}
 assert all(not item['dirty'] for item in other.values())
 assert other['theme']['head']=='5acd1b6b66c02f61d3216e3adace5dd9e0404fc9' and other['liquid']['head']=='4e39ae4cc3da73921923c0669e0fc84a66b2f696'
 changed_ruby=subprocess.check_output(['git','-C',str(args.ruby_root),'diff','--name-only','7db1cc962c1a90a40613da98fdff18f061680bb8','HEAD'],text=True).splitlines()
 assert not any(path.startswith(('lib/','bin/','test/','proto/','fixtures/')) or path in ('Gemfile','Gemfile.lock','benchmark/serve_worker.rb') for path in changed_ruby),changed_ruby
 env=os.environ.copy()
 for k in ('RUBYLIB','RUBYOPT','RUBY_YJIT_ENABLE','GEM_HOME','GEM_PATH','BUNDLE_PATH','HORIZON_DATA_TOKEN','HORIZON_STORE_TOKEN','HORIZON_PROFILE_ALLOCATIONS','HORIZON_PROFILE_HOST'):env.pop(k,None)
 env.update(BUNDLE_GEMFILE=str(args.ruby_root/'Gemfile'),GEM_HOME=str(args.ruby_gem_home),GEM_PATH=str(args.ruby_gem_home))
 shared=['--theme-root',str(args.theme_root),'--fixture',str(args.fixture),'--scope','page','--page','index','--warmup',str(args.warmup),'--serve-stdio']
 commands={v['name']:[v['binary'],*shared] for v in manifest['variants']}
 commands['ruby_yjit']=[str(args.ruby),'--yjit','-rbundler/setup',str(args.ruby_root/'benchmark/serve_worker.rb'),'--liquid-root',str(args.liquid_root),'--theme-root',str(args.theme_root),'--fixture',str(args.fixture),'--warmup',str(args.warmup),'--mode','direct']
 inputs={k:fingerprint(v) for k,v in [('fixture',args.fixture),('ruby',args.ruby),('script',Path(__file__)),('manifest',args.manifest)]}
 assert inputs['fixture']['sha256']=='867c41e0929881290af2b261af64146632bf0287524f5a6c55714af32e819f98'
 assert sorted(os.sched_getaffinity(0))==[args.controller_cpu]
 rng=random.Random(args.seed); runs=[]; started=time.time(); load_start=list(os.getloadavg())
 for batch in range(args.batches):
  names=list(commands); rng.shuffle(names)
  for order,name in enumerate(names):runs.append(await run(name,commands[name],args,env,batch,order))
 assert inputs=={k:fingerprint(v) for k,v in [('fixture',args.fixture),('ruby',args.ruby),('script',Path(__file__)),('manifest',args.manifest)]}
 assert sources=={v['name']:archive_state(v) for v in manifest['variants']}
 assert other=={k:common.repository(root.resolve()) for k,root in source_roots.items()}
 summary={}
 for name in commands:
  selected=[x for x in runs if x['variant']==name]; vals=[x['worker_cpu_ms_per_request'] for x in selected]
  summary[name]={'batch_worker_cpu_ms_per_request':vals,'median_worker_cpu_ms_per_request':statistics.median(vals),'min_worker_cpu_ms_per_request':min(vals),'max_worker_cpu_ms_per_request':max(vals),'batch_median_pipe_wall_ms':[x['median_pipe_wall_ms'] for x in selected]}
 result={'schema_version':1,'correctness_verified':True,'started_unix':started,'completed_unix':time.time(),'sources':sources,'other_sources':other,'inputs':inputs,'configuration':{k:getattr(args,k) for k in ('batches','iterations','warmup','worker_cpu','controller_cpu','seed')},'controller_affinity':sorted(os.sched_getaffinity(0)),'cpu_tick_seconds':1/os.sysconf('SC_CLK_TCK'),'artifacts':EXPECTED,'load_start':load_start,'load_end':list(os.getloadavg()),'summary':summary,'runs':runs,'limits':['Worker CPU diagnostic, not HTTP RPS or render-only wall latency.','Linux /proc/pid/stat user+system delta after READY/warmup through final verified response; includes rendering, JSON framing and pipe writes, excludes startup/warmup and parent verification CPU.','10ms CPU ticks averaged over the configured measured requests. Serial seeded randomized variants; affinity does not reserve cores; host load/frequency uncontrolled.','Candidates are separately frozen source archives based on the stated commit; every tracked file, changed source, patch and executable is verified before and after. No timing instrumentation in these binaries.']}
 (args.output_dir/'cpu.json').write_text(json.dumps(result,indent=2)+'\n'); print(json.dumps(summary),flush=True)

if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__)
 for k in ('manifest','ruby','ruby-root','ruby-gem-home','rust-root','liquid-root','theme-root','fixture','output-dir'):p.add_argument('--'+k,type=Path,required=True)
 p.add_argument('--batches',type=int,default=3);p.add_argument('--iterations',type=int,default=100);p.add_argument('--warmup',type=int,default=50);p.add_argument('--worker-cpu',type=int,default=2);p.add_argument('--controller-cpu',type=int,default=0);p.add_argument('--seed',type=int,default=20261002)
 args=p.parse_args(); assert args.batches>=3 and args.iterations>=100 and args.warmup>=50
 asyncio.run(main(args))
