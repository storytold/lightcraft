#!/usr/bin/env python3
"""Install verified DSRNet/VGG19 weights for LightCraft's native Rust inference.
Python is needed for this one-time conversion only, never by the running app.
"""
import argparse, hashlib, json, pathlib, shutil, tempfile, urllib.request, os
S_SHA='3d85041c53c025511c654b1b42da35a404d6c76cb1fb790a2adad4028875554c'
L_SHA='0e6a825e7342a9166eafea8e7714870d6decb3dae2febea603bb67a78341251f'
NATURE_SHA='09a7acdb3efef6ad7065164e205bf5173a016ce8b24fc56d9751c8791cf65cff'
VGG_SHA='dcbb9e9dad569fff7a846263a77324fc34978fea2bfb039c012d710e1776ae44'
S_URL='https://raw.githubusercontent.com/mingcv/DSRNet/7953e4045f89dd0b76d63bd3ac3eb4946f5cabc6/weights/dsrnet_s_epoch14.pt'
VGG_URL='https://download.pytorch.org/models/vgg19-dcbb9e9d.pth'
def digest(path):
 h=hashlib.sha256()
 with open(path,'rb') as f:
  for b in iter(lambda:f.read(1<<20),b''):h.update(b)
 return h.hexdigest()
def fetch(url,path,sha):
 if path.exists() and digest(path)==sha:return path
 path.parent.mkdir(parents=True,exist_ok=True)
 tmp=path.with_suffix(path.suffix+'.part')
 try:
  with urllib.request.urlopen(url,timeout=60) as src,open(tmp,'wb') as dst:shutil.copyfileobj(src,dst)
  if digest(tmp)!=sha:raise ValueError('Downloaded checkpoint checksum mismatch')
  os.replace(tmp,path)
 finally:tmp.unlink(missing_ok=True)
 return path
def main():
 p=argparse.ArgumentParser(description=__doc__)
 p.add_argument('--model',choices=['dsrnet-s','dsrnet-l','dsrnet-l-nature'],default='dsrnet-s')
 p.add_argument('--checkpoint',type=pathlib.Path);p.add_argument('--checkpoint-sha256',help='Required expected SHA256 for a user-supplied large checkpoint')
 p.add_argument('--vgg',type=pathlib.Path);p.add_argument('--output',required=True,type=pathlib.Path)
 p.add_argument('--download',action='store_true',help='Fetch the official model and VGG19 (about 590 MB for S or 1.05 GB for L variants)')
 a=p.parse_args()
 if a.output.exists():p.error('Output directory already exists; choose a new directory')
 cache=pathlib.Path.home()/'.cache/lightcraft-reflections-convert'
 checkpoint=a.checkpoint
 if checkpoint is None and a.download:
  if a.model=='dsrnet-s':checkpoint=fetch(S_URL,cache/'dsrnet_s_epoch14.pt',S_SHA)
  else:
   import gdown
   nature=a.model=='dsrnet-l-nature';sha=NATURE_SHA if nature else L_SHA
   checkpoint=cache/('dsrnet_l_4000_epoch33.pt' if nature else 'dsrnet-l.pt');cache.mkdir(parents=True,exist_ok=True)
   if not checkpoint.exists() or digest(checkpoint)!=sha:
    tmp=checkpoint.with_suffix('.part')
    try:
     gdown.download(id='19His-4uV0C_fteSOI_mBL8vyQn_Fdh_8' if nature else '12w8m5KTuVuwGA-x2Di87wj6fQTScJgGz',output=str(tmp),quiet=False)
     if not tmp.exists() or digest(tmp)!=sha:raise ValueError('Checkpoint SHA256 mismatch')
     os.replace(tmp,checkpoint)
    finally:tmp.unlink(missing_ok=True)
 vgg=a.vgg or (fetch(VGG_URL,cache/'vgg19-dcbb9e9d.pth',VGG_SHA) if a.download else None)
 if checkpoint is None or vgg is None:p.error('Supply --checkpoint and --vgg, or choose --download for DSRNet-S')
 expected=S_SHA if a.model=='dsrnet-s' else NATURE_SHA if a.model=='dsrnet-l-nature' else a.checkpoint_sha256 or L_SHA
 if not expected or len(expected)!=64:p.error('Supply the verified --checkpoint-sha256 for the large checkpoint')
 if digest(checkpoint)!=expected.lower() or digest(vgg)!=VGG_SHA:p.error('Checkpoint SHA256 mismatch; no pickle was loaded')
 import torch
 from safetensors.torch import save_file
 # Restrict pickle loading to tensor weights; never trust serialized Python modules.
 state=torch.load(checkpoint,map_location='cpu',weights_only=True)
 if 'state_dict' in state:state=state['state_dict']
 if 'icnn' in state:state=state['icnn']
 if not isinstance(state,dict):raise ValueError('Expected tensor state dict')
 tensors={k.removeprefix('module.'):v.detach().float().contiguous() for k,v in state.items() if isinstance(v,torch.Tensor) and not k.removeprefix('module.').startswith('lrm.')}
 small=a.model=='dsrnet-s';width=32 if small else 64
 if tuple(tensors.get('ending.seq.0.weight',torch.empty(0)).shape)!=(3,width,3,3):raise ValueError('Checkpoint architecture does not match selected model')
 vgg_state=torch.load(vgg,map_location='cpu',weights_only=True)
 for k,v in vgg_state.items():
  if k.startswith('features.') and int(k.split('.')[1])<30:tensors['vgg.'+k[len('features.'):]]=v.detach().float().contiguous()
 if not all(torch.isfinite(v).all().item() for v in tensors.values()):raise ValueError('Non-finite model weights')
 a.output.parent.mkdir(parents=True,exist_ok=True)
 tmp=pathlib.Path(tempfile.mkdtemp(prefix='.reflection-install-',dir=a.output.parent))
 try:
  model=tmp/'model.safetensors';save_file(tensors,str(model))
  manifest={'version':1,'architecture':a.model,'sha256':digest(model),'source_sha256':digest(checkpoint),'vgg_sha256':VGG_SHA}
  (tmp/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
  # Exclusive destination: never replace an existing installation.
  if a.output.exists():raise FileExistsError(a.output)
  os.rename(tmp,a.output)
 finally:
  if tmp.exists():shutil.rmtree(tmp)
 print(f'Installed {a.model}: {a.output} ({len(tensors)} tensors)')
if __name__=='__main__':main()
