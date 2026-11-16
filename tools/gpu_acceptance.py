"""PICA stop/frozen-draw/shader/surface acceptance on software and offscreen Vulkan."""
import argparse
import json
from pathlib import Path
from native_debug_probe import Peer

parser=argparse.ArgumentParser(__doc__)
parser.add_argument("worker",type=Path)
parser.add_argument("rom",type=Path)
parser.add_argument("output",type=Path)
args=parser.parse_args()
for new in (False,True):
    for vulkan in (False,True):
        root=args.output.resolve()/f"{'new' if new else 'old'}-{'vulkan' if vulkan else 'software'}"
        peer=Peer(args.worker.resolve(),root)
        try:
            peer.call("hello",{"notifications":True})
            for name,value in {"is_new_3ds":new,"graphics_api":2 if vulkan else 0,"use_hw_shader":False}.items():
                peer.debug("settings_set",name=name,value=value)
            peer.call("load",{"path":str(args.rom.resolve())})
            peer.call("run_until",{"frames":60})
            peer.debug("gpu_configure",enabled=True,stop_events=[2])
            stopped=peer.call("run_until",{"frames":1})
            assert stopped["stop"]["reason"]=="pica" and stopped["stop"]["event"]==2,stopped
            draw=stopped["stop"]["draw"]
            listing=peer.debug("gpu_draws")
            entry=next(row for row in listing["draws"] if row["draw"]==draw)
            assert entry["vertices"]>0,listing
            capture=peer.debug("gpu_draw",draw=draw)
            frozen=json.loads((root/capture["artifacts"][0]["path"]).read_text())
            assert any(r.get("kind")=="vertex" for r in frozen["resources"]),frozen["resources"]
            assert frozen["vertices"] and all("output_f32_bits" in v for v in frozen["vertices"])
            assert frozen["draw"]==draw and frozen["command"]==stopped["stop"]["command"],frozen
            trace=peer.debug("shader_trace",draw=draw,vertex=0)
            instructions=json.loads((root/trace["artifacts"][0]["path"]).read_text())
            assert instructions["matches_frozen_output"],instructions
            assert 0<trace["instructions"]<=8192 and not instructions["truncated"],trace
            assert any("dest_f32_bits" in row for row in instructions["instructions"]),instructions
            surfaces=[]
            for texture in frozen["resources"]:
                if "error" in texture or texture.get("kind")!="texture":continue
                surface=peer.debug("surface",address=texture["address"],width=texture["width"],
                                   height=texture["height"],format=texture["format"])
                artifact=surface["artifacts"][0]
                assert (root/artifact["path"]).stat().st_size==texture["width"]*texture["height"]*4
                surfaces.append({key:surface[key] for key in ("address","width","height","source_xxh3_64")})
            texture=next(r for r in frozen["resources"] if r.get("kind")=="texture")
            def next_matching_texture():
                for attempt in range(128):
                    peer.call("run_until",{"frames":1})
                    newest=peer.debug("gpu_draws")["draws"][-1]["draw"]
                    snapshot=json.loads((root/peer.debug("gpu_draw",draw=newest)["artifacts"][0]["path"]).read_text())
                    found=next((r for r in snapshot["resources"] if r.get("kind")=="texture" and r["address"]==texture["address"]),None)
                    if found:return found
                raise AssertionError("texture not used again within 128 draws")
            same=next_matching_texture()
            assert same["version"]==texture["version"] and same["content_xxh3_64"]==texture["content_xxh3_64"]
            physical=texture["address"]
            assert texture["virtual_aliases"],texture
            virtual=texture["virtual_aliases"][0]
            before=peer.debug("read_memory",address=virtual,length=4)["bytes"]
            changed=before.copy();changed[0]^=255
            peer.debug("write_memory",address=virtual,bytes=changed)
            modified=next_matching_texture()
            assert modified["version"]!=same["version"] and modified["content_xxh3_64"]!=same["content_xxh3_64"]
            assert frozen["resources"][0]["data_hex"]==texture["data_hex"]
            peer.debug("write_memory",address=virtual,bytes=before)
            report={"resource_versions":[texture["version"],same["version"],modified["version"]],"new_3ds":new,"renderer":"vulkan" if vulkan else "software","stop":stopped["stop"],
                "draw":entry,"shader_instructions":trace["instructions"],"surfaces":surfaces}
            (root/"acceptance.json").write_text(json.dumps(report,indent=2))
            print(json.dumps(report),flush=True)
        finally:peer.close()
