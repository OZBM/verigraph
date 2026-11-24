# Hero picture for the VeriGraph brochure.
# Run headless: Blender -b --factory-startup -P hero.py -- out.png W H SAMPLES
# A dataflow graph of program pieces; a green scan line has proven everything
# behind it (green), caught one bug (coral), and the pieces ahead are drafts (glass).
import bpy, sys, math
from mathutils import Vector

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
OUT = argv[0] if argv else "/tmp/hero.png"
W = int(argv[1]) if len(argv) > 1 else 800
H = int(argv[2]) if len(argv) > 2 else 600
SPP = int(argv[3]) if len(argv) > 3 else 32

scene = bpy.context.scene
for o in list(bpy.data.objects):
    bpy.data.objects.remove(o, do_unlink=True)

# ---------- render ----------
scene.render.engine = 'CYCLES'
try:
    prefs = bpy.context.preferences.addons['cycles'].preferences
    prefs.compute_device_type = 'METAL'
    prefs.get_devices()
    for d in prefs.devices:
        d.use = True
    scene.cycles.device = 'GPU'
except Exception as ex:
    print("GPU setup failed:", ex)
scene.cycles.samples = SPP
scene.view_settings.exposure = 0.25
scene.cycles.use_denoising = True
scene.cycles.max_bounces = 10
scene.cycles.transmission_bounces = 10
scene.render.resolution_x, scene.render.resolution_y = W, H
scene.render.resolution_percentage = 100
scene.render.image_settings.file_format = 'PNG'
try:
    scene.view_settings.view_transform = 'AgX'
    scene.view_settings.look = 'AgX - Medium High Contrast'
except Exception as ex:
    print("look:", ex)

# ---------- palette (linear RGB) ----------
NAVY = (0.006, 0.011, 0.028)
GREEN = (0.10, 0.95, 0.45)
CYAN = (0.12, 0.42, 1.00)
CORAL = (1.00, 0.22, 0.12)
WHITE = (0.82, 0.85, 0.90)


def set_in(node, names, value):
    for n in names:
        if n in node.inputs:
            node.inputs[n].default_value = value
            return


def principled(name, base, rough=0.4, coat=0.0, transmission=0.0, ior=1.45):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    p = next(n for n in m.node_tree.nodes if n.type == 'BSDF_PRINCIPLED')
    set_in(p, ["Base Color"], (*base, 1))
    set_in(p, ["Roughness"], rough)
    set_in(p, ["Coat Weight", "Clearcoat"], coat)
    set_in(p, ["Transmission Weight", "Transmission"], transmission)
    set_in(p, ["IOR"], ior)
    return m


def emission(name, color, strength):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    nt.nodes.clear()
    e = nt.nodes.new('ShaderNodeEmission')
    e.inputs['Color'].default_value = (*color, 1)
    e.inputs['Strength'].default_value = strength
    o = nt.nodes.new('ShaderNodeOutputMaterial')
    nt.links.new(e.outputs[0], o.inputs[0])
    return m


M_TILE = principled("tile", WHITE, rough=0.32, coat=0.6)
M_GLASS = principled("glass", (0.80, 0.88, 1.0), rough=0.22, transmission=1.0)
M_FLOOR = principled("floor", (0.010, 0.013, 0.024), rough=0.2)
M_EDGE = emission("edge", CYAN, 2.6)
M_EDGE_DRAFT = emission("edge_draft", (0.35, 0.42, 0.60), 0.5)
M_PACKET = emission("packet", (0.75, 0.90, 1.0), 25.0)
M_LED = {"ok": emission("led_ok", GREEN, 30.0), "bug": emission("led_bug", CORAL, 45.0),
         "draft": principled("led_off", (0.05, 0.06, 0.08), rough=0.3)}

# ---------- world and floor ----------
world = bpy.data.worlds.new("w")
scene.world = world
world.use_nodes = True
bg = next(n for n in world.node_tree.nodes if n.type == 'BACKGROUND')
bg.inputs['Color'].default_value = (*NAVY, 1)
bg.inputs['Strength'].default_value = 1.0

bpy.ops.mesh.primitive_plane_add(size=200, location=(0, 0, 0))
bpy.context.active_object.data.materials.append(M_FLOOR)

# ---------- the graph ----------
TW, TD, TH, TZ = 1.75, 1.08, 0.36, 0.62
SCAN_X = 8.15
NODES = {
    "A": (0.0, -2.3), "B": (0.0, 0.0), "C": (0.0, 2.3),
    "D": (3.2, -1.25), "E": (3.2, 1.35),
    "F": (6.4, -2.45), "G": (6.4, 0.15), "H": (6.4, 2.65),
    "I": (9.6, -1.05), "J": (9.6, 1.6),
    "K": (12.8, 0.3),
}
BUG = "F"
EDGES = ["AD", "BD", "BE", "CE", "DF", "DG", "EG", "EH", "FI", "GI", "GJ", "HJ", "IK", "JK"]


def state(k):
    if k == BUG:
        return "bug"
    return "ok" if NODES[k][0] < SCAN_X else "draft"


for k, (x, y) in NODES.items():
    s = state(k)
    bpy.ops.mesh.primitive_cube_add(size=1, location=(x, y, TZ))
    t = bpy.context.active_object
    t.name = "tile_" + k
    t.scale = (TW, TD, TH)
    bpy.ops.object.transform_apply(scale=True)
    bev = t.modifiers.new("bevel", 'BEVEL')
    bev.width, bev.segments, bev.limit_method = 0.15, 8, 'NONE'
    try:
        bev.harden_normals = True
    except Exception:
        pass
    bpy.ops.object.shade_smooth()
    t.data.materials.append(M_GLASS if s == "draft" else M_TILE)
    # status light on the top face
    bpy.ops.mesh.primitive_cylinder_add(radius=0.085, depth=0.03,
                                        location=(x + TW / 2 - 0.3, y + TD / 2 - 0.27, TZ + TH / 2 + 0.01))
    bpy.context.active_object.data.materials.append(M_LED[s])
    # glow pool on the floor under checked pieces
    if s != "draft":
        L = bpy.data.lights.new("glow_" + k, 'POINT')
        L.color = GREEN if s == "ok" else CORAL
        L.energy = 55 if s == "ok" else 90
        L.shadow_soft_size = 0.6
        lo = bpy.data.objects.new("glow_" + k, L)
        lo.location = (x, y, 0.16)
        scene.collection.objects.link(lo)


def bez(p0, p1, p2, p3, t):
    u = 1 - t
    return p0 * u ** 3 + p1 * 3 * u * u * t + p2 * 3 * u * t * t + p3 * t ** 3


bpy.ops.mesh.primitive_uv_sphere_add(radius=0.075, segments=24, ring_count=12, location=(0, 0, -5))
packet = bpy.context.active_object
packet.data.materials.append(M_PACKET)

for i, e in enumerate(EDGES):
    (sx, sy), (tx, ty) = NODES[e[0]], NODES[e[1]]
    oy = max(-0.3, min(0.3, (ty - sy) * 0.12))
    p0 = Vector((sx + TW / 2 - 0.06, sy + oy, TZ))
    p3 = Vector((tx - TW / 2 + 0.06, ty - oy, TZ))
    dx = (p3.x - p0.x) * 0.55
    p1, p2 = p0 + Vector((dx, 0, 0)), p3 - Vector((dx, 0, 0))
    draft = state(e[1]) == "draft" and state(e[0]) == "draft"
    cu = bpy.data.curves.new("edge_" + e, 'CURVE')
    cu.dimensions = '3D'
    cu.bevel_depth = 0.022 if draft else 0.034
    cu.bevel_resolution = 6
    cu.resolution_u = 48
    sp = cu.splines.new('BEZIER')
    sp.bezier_points.add(1)
    a, b = sp.bezier_points
    a.co, a.handle_left, a.handle_right = p0, p0 - Vector((dx, 0, 0)), p1
    b.co, b.handle_left, b.handle_right = p3, p2, p3 + Vector((dx, 0, 0))
    cu.materials.append(M_EDGE_DRAFT if draft else M_EDGE)
    ob = bpy.data.objects.new("edge_" + e, cu)
    scene.collection.objects.link(ob)
    if not draft:
        for t in ((0.28, 0.72) if i % 2 else (0.5,)):
            c = packet.copy()
            c.location = bez(p0, p1, p2, p3, t)
            scene.collection.objects.link(c)

# ---------- the scan line ----------
bpy.ops.mesh.primitive_plane_add(size=1, location=(SCAN_X, 0.15, 1.25))
sc = bpy.context.active_object
sc.rotation_euler = (0, math.radians(90), 0)
sc.scale = (2.5, 9.6, 1)
m = bpy.data.materials.new("scan")
m.use_nodes = True
nt = m.node_tree
nt.nodes.clear()
tc = nt.nodes.new('ShaderNodeTexCoord')
sep = nt.nodes.new('ShaderNodeSeparateXYZ')
pw = nt.nodes.new('ShaderNodeMath'); pw.operation = 'POWER'; pw.inputs[1].default_value = 1.6
ends = nt.nodes.new('ShaderNodeValToRGB')
el = ends.color_ramp.elements
el[0].position, el[0].color = 0.0, (0, 0, 0, 1)
el[1].position, el[1].color = 0.18, (1, 1, 1, 1)
e3 = el.new(0.82); e3.color = (1, 1, 1, 1)
e4 = el.new(1.0); e4.color = (0, 0, 0, 1)
mul = nt.nodes.new('ShaderNodeMath'); mul.operation = 'MULTIPLY'
em = nt.nodes.new('ShaderNodeEmission')
em.inputs['Color'].default_value = (*GREEN, 1)
em.inputs['Strength'].default_value = 3.2
tr = nt.nodes.new('ShaderNodeBsdfTransparent')
mix = nt.nodes.new('ShaderNodeMixShader')
out = nt.nodes.new('ShaderNodeOutputMaterial')
nt.links.new(tc.outputs['Generated'], sep.inputs[0])
nt.links.new(sep.outputs['X'], pw.inputs[0])  # local +X points down after the rotation
nt.links.new(sep.outputs['Y'], ends.inputs['Fac'])
nt.links.new(pw.outputs[0], mul.inputs[0])
nt.links.new(ends.outputs['Color'], mul.inputs[1])
nt.links.new(mul.outputs[0], mix.inputs['Fac'])
nt.links.new(tr.outputs[0], mix.inputs[1])
nt.links.new(em.outputs[0], mix.inputs[2])
nt.links.new(mix.outputs[0], out.inputs[0])
sc.data.materials.append(m)

bpy.ops.mesh.primitive_cube_add(size=1, location=(SCAN_X, 0.15, 0.006))
ln = bpy.context.active_object
ln.scale = (0.05, 9.6, 0.012)
ln.data.materials.append(emission("scanline", GREEN, 14.0))

L = bpy.data.lights.new("scan_light", 'AREA')
L.shape, L.size, L.size_y = 'RECTANGLE', 0.3, 9.0
L.color, L.energy = GREEN, 260
lo = bpy.data.objects.new("scan_light", L)
lo.location = (SCAN_X - 0.1, 0.15, 1.2)
lo.rotation_euler = (0, math.radians(-90), 0)  # shine back over the proven side
scene.collection.objects.link(lo)

# ---------- key and rim lights ----------
def area(name, loc, target, size, energy, color):
    L = bpy.data.lights.new(name, 'AREA')
    L.size, L.energy, L.color = size, energy, color
    o = bpy.data.objects.new(name, L)
    o.location = loc
    o.rotation_euler = (Vector(target) - Vector(loc)).to_track_quat('-Z', 'Y').to_euler()
    o.visible_glossy = name != "rim"  # its floor reflection read as a stray window
    scene.collection.objects.link(o)


area("key", (1.5, -7.5, 9.5), (6.4, 0, 0.5), 7.0, 650, (1.0, 0.97, 0.93))
area("rim", (10.0, 9.0, 4.5), (6.4, 0, 0.5), 6.0, 900, (0.45, 0.62, 1.0))

# ---------- camera ----------
cam = bpy.data.objects.new("cam", bpy.data.cameras.new("cam"))
scene.collection.objects.link(cam)
scene.camera = cam
cam.location = (6.4, -13.6, 7.4)
target = Vector((6.4, 0.5, 0.25))
cam.rotation_euler = (target - cam.location).to_track_quat('-Z', 'Y').to_euler()
cam.data.lens = 30
cam.data.dof.use_dof = True
cam.data.dof.focus_distance = (Vector((SCAN_X - 1.5, -1.0, TZ)) - cam.location).length
cam.data.dof.aperture_fstop = 3.2

scene.render.filepath = OUT
bpy.ops.render.render(write_still=True)
print("WROTE", OUT)
