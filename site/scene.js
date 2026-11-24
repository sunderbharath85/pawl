// The hero scene: a brass ratchet wheel, a steel pawl that rides each tooth and snaps back,
// and a pet walking on top of the wheel. Exposes `state.t` (0 = hero layout, 1 = side layout)
// and `setScrollVelocity` so the page can drive it from scroll.
import * as THREE from "three";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";

const TEETH = 36;
const R1 = 1.0; // root radius
const R2 = 1.07; // tip radius
const STEP = (Math.PI * 2) / TEETH;
const RISE = 0.12; // share of a tooth taken by the steep face
const SPECIES = ["dog", "cat", "bird", "robot"];

function wheelShape() {
  const s = new THREE.Shape();
  for (let i = 0; i < TEETH; i++) {
    const a0 = i * STEP;
    const pts = [[R1, a0], [R2, a0 + RISE * STEP]];
    for (let k = 1; k <= 8; k++) {
      const t = k / 8;
      pts.push([R2 - (R2 - R1) * Math.pow(t, 1.15), a0 + (RISE + (1 - RISE) * t) * STEP]);
    }
    pts.forEach(([r, a], j) => {
      const x = r * Math.cos(a);
      const y = r * Math.sin(a);
      if (i === 0 && j === 0) s.moveTo(x, y);
      else s.lineTo(x, y);
    });
  }
  const hole = (x, y, r) => {
    const p = new THREE.Path();
    p.absarc(x, y, r, 0, Math.PI * 2, true);
    return p;
  };
  s.holes.push(hole(0, 0, 0.13));
  for (let i = 0; i < 6; i++) {
    const a = (i / 6) * Math.PI * 2 + Math.PI / 6;
    s.holes.push(hole(Math.cos(a) * 0.6, Math.sin(a) * 0.6, 0.17));
  }
  return s;
}

// Rim radius under a fixed point at local angle `phi` (wheel frame).
function rimAt(phi) {
  const u = (((phi % STEP) + STEP) % STEP) / STEP;
  if (u < RISE) return R1 + (R2 - R1) * (u / RISE);
  return R2 - (R2 - R1) * Math.pow((u - RISE) / (1 - RISE), 1.15);
}

export function createScene(canvas, { reduced = false } = {}) {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: "high-performance" });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.0;
  renderer.outputColorSpace = THREE.SRGBColorSpace;

  const scene = new THREE.Scene();
  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.55;

  const camera = new THREE.PerspectiveCamera(30, 1, 0.1, 100);
  camera.position.set(0, 0, 9);

  const key = new THREE.DirectionalLight(0xffd2a1, 2.6);
  key.position.set(-4, 5, 6);
  const rim = new THREE.DirectionalLight(0x9bb6ff, 2.2);
  rim.position.set(5, 3, -4);
  const glow = new THREE.PointLight(0xe2703a, 10, 14, 2);
  glow.position.set(0, -1, 3.5);
  scene.add(key, rim, glow);

  const brass = new THREE.MeshPhysicalMaterial({ color: 0xc99258, metalness: 1, roughness: 0.3, clearcoat: 0.5, clearcoatRoughness: 0.22 });
  const darkBrass = new THREE.MeshPhysicalMaterial({ color: 0x8f5d31, metalness: 1, roughness: 0.38 });
  const steel = new THREE.MeshPhysicalMaterial({ color: 0xc3c8cf, metalness: 1, roughness: 0.24, clearcoat: 0.3 });

  // ---- rig: everything that moves between layouts
  const rig = new THREE.Group();
  scene.add(rig);

  const wheelGeo = new THREE.ExtrudeGeometry(wheelShape(), {
    depth: 0.16, bevelEnabled: true, bevelThickness: 0.025, bevelSize: 0.016, bevelSegments: 3, curveSegments: 40,
  });
  wheelGeo.translate(0, 0, -0.08);
  const wheel = new THREE.Mesh(wheelGeo, brass);
  rig.add(wheel);

  const hub = new THREE.Mesh(new THREE.CylinderGeometry(0.3, 0.3, 0.3, 64), darkBrass);
  hub.rotation.x = Math.PI / 2;
  wheel.add(hub);
  const axle = new THREE.Mesh(new THREE.CylinderGeometry(0.11, 0.11, 0.42, 48), steel);
  axle.rotation.x = Math.PI / 2;
  rig.add(axle);
  const ring = new THREE.Mesh(new THREE.TorusGeometry(0.82, 0.018, 16, 160), darkBrass);
  ring.position.z = 0.1;
  wheel.add(ring);

  // ---- pawl: the tip rests on the rim at angle ALPHA (upper right, beside the pet); the pivot
  // trails behind it, so the pawl points along the free direction like a door wedge.
  const ALPHA = THREE.MathUtils.degToRad(68);
  const radialA = new THREE.Vector2(Math.cos(ALPHA), Math.sin(ALPHA));
  const tangentA = new THREE.Vector2(-Math.sin(ALPHA), Math.cos(ALPHA));
  const tip0 = radialA.clone().multiplyScalar(R1);
  const pivot = tip0.clone().addScaledVector(radialA, 0.3).addScaledVector(tangentA, -0.5);
  const arm = tip0.clone().sub(pivot);
  const L = arm.length();
  const armAngle = Math.atan2(arm.y, arm.x);
  const perp = new THREE.Vector2(-arm.y, arm.x).normalize();
  const liftDir = Math.sign(perp.dot(radialA)) || 1; // which way the pawl turns to lift its tip
  const liftPerRad = L * Math.abs(perp.dot(radialA)); // rim rise per radian of pawl turn

  const ps = new THREE.Shape();
  ps.moveTo(0, 0.12);
  ps.lineTo(L * 0.72, 0.075);
  ps.lineTo(L + 0.035, 0.0);
  ps.lineTo(L * 0.8, -0.07);
  ps.lineTo(0, -0.12);
  ps.absarc(0, 0, 0.12, -Math.PI / 2, Math.PI / 2, true);
  const pin = new THREE.Path();
  pin.absarc(0, 0, 0.045, 0, Math.PI * 2, true);
  ps.holes.push(pin);
  const pawlGeo = new THREE.ExtrudeGeometry(ps, { depth: 0.11, bevelEnabled: true, bevelThickness: 0.018, bevelSize: 0.012, bevelSegments: 3, curveSegments: 32 });
  pawlGeo.translate(0, 0, -0.055);
  const pawlPivot = new THREE.Group();
  pawlPivot.position.set(pivot.x, pivot.y, 0);
  const pawl = new THREE.Mesh(pawlGeo, steel);
  pawlPivot.add(pawl);
  rig.add(pawlPivot);
  const pivotPin = new THREE.Mesh(new THREE.CylinderGeometry(0.05, 0.05, 0.3, 32), darkBrass);
  pivotPin.rotation.x = Math.PI / 2;
  pivotPin.position.set(pivot.x, pivot.y, 0);
  rig.add(pivotPin);

  // Spring: a unit helix along +y, stretched between an anchor and a point on the pawl.
  const helixPts = [];
  for (let i = 0; i <= 220; i++) {
    const t = i / 220;
    const a = t * Math.PI * 2 * 9;
    helixPts.push(new THREE.Vector3(Math.cos(a) * 0.045, t, Math.sin(a) * 0.045));
  }
  const spring = new THREE.Mesh(
    new THREE.TubeGeometry(new THREE.CatmullRomCurve3(helixPts), 440, 0.008, 6, false),
    steel,
  );
  rig.add(spring);
  const armMid = pivot.clone().add(tip0).multiplyScalar(0.5);
  const anchor2 = armMid.clone().addScaledVector(radialA, 0.42);
  const anchor = new THREE.Vector3(anchor2.x, anchor2.y, 0);
  const anchorCap = new THREE.Mesh(new THREE.CylinderGeometry(0.04, 0.04, 0.22, 24), darkBrass);
  anchorCap.rotation.x = Math.PI / 2;
  anchorCap.position.copy(anchor);
  rig.add(anchorCap);
  // attach the spring on the outward edge of the arm, halfway along (pawl space)
  const outwardLocal = radialA.clone().rotateAround(new THREE.Vector2(), -armAngle);
  const springLocal = new THREE.Vector3(L * 0.5, Math.sign(outwardLocal.y) * 0.07, 0);

  // ---- the pet on top
  const loader = new THREE.TextureLoader();
  const textures = {};
  const petMat = new THREE.MeshBasicMaterial({ transparent: true, alphaTest: 0.04, depthWrite: false, toneMapped: false });
  const pet = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), petMat);
  pet.renderOrder = 2;
  scene.add(pet);
  function texture(species) {
    if (!textures[species]) {
      const t = loader.load(`pets/${species}/walk.png`, () => requestRender());
      t.colorSpace = THREE.SRGBColorSpace;
      t.repeat.set(0.25, 1);
      t.anisotropy = 4;
      textures[species] = t;
    }
    return textures[species];
  }
  let speciesIndex = 0;
  petMat.map = texture(SPECIES[0]);
  SPECIES.slice(1).forEach(texture);

  // ---- state
  const state = { t: 0 }; // tweened by the page
  let theta = 0;
  let omega = 0;
  let scrollVel = 0;
  let walk = 0;
  let pawlAng = 0;
  let pawlVel = 0;
  let hop = 0;
  const HOP = 0.45;
  let hopTime = 0;
  let blocked = 0;
  let flash = 0;
  const pointer = new THREE.Vector2();
  const pointerSmooth = new THREE.Vector2();
  let visibleH = 1;
  let visibleW = 1;
  let running = false;
  let needsRender = true;
  let last = performance.now();
  const delta = () => {
    const now = performance.now();
    const d = (now - last) / 1000;
    last = now;
    return d;
  };

  function resize() {
    const w = window.innerWidth;
    const h = window.innerHeight;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    visibleH = 2 * camera.position.z * Math.tan(THREE.MathUtils.degToRad(camera.fov / 2));
    visibleW = visibleH * camera.aspect;
    requestRender();
  }

  const lerp = THREE.MathUtils.lerp;
  const ease = (x) => x * x * (3 - 2 * x);

  function layout() {
    const t = ease(Math.min(Math.max(state.t, 0), 1));
    const narrow = camera.aspect < 0.85;
    // hero: a huge wheel rising from the bottom edge, pet walking on its crest
    const heroScale = narrow ? 1.65 : 2.35;
    const heroY = -visibleH / 2 - heroScale * R2 + (narrow ? 0.55 : 0.42);
    // side: smaller, to the right of the text, tilted to show its depth
    const sideScale = narrow ? 1.0 : 1.35;
    const sideX = narrow ? 0 : visibleW * 0.27;
    const sideY = narrow ? -visibleH * 0.28 : -0.25;
    const s = lerp(heroScale, sideScale, t);
    rig.scale.setScalar(s);
    rig.position.set(lerp(0, sideX, t), lerp(heroY, sideY, t), 0);
    rig.rotation.set(lerp(0, 0.18, t), lerp(0, -0.42, t), 0);

    const petH = (narrow ? 0.62 : 0.82) * lerp(1, 0.85, t);
    pet.scale.set(petH, petH, 1);
    rig.updateMatrixWorld(true);
    const top = new THREE.Vector3(0, R2, 0.06).applyMatrix4(rig.matrixWorld);
    pet.position.set(top.x, top.y + petH * 0.44 + hop, top.z + 0.05);
  }

  function update(dt) {
    const idle = reduced ? 0 : 0.32;
    const push = scrollVel * 0.0016;
    const target = Math.max(idle + push, 0);
    blocked = push < -idle - 0.05 ? Math.min(1, blocked + dt * 6) : Math.max(0, blocked - dt * 4);
    omega = lerp(omega, target, 1 - Math.exp(-dt * 6));
    theta += omega * dt;
    wheel.rotation.z = theta + (blocked ? Math.sin(performance.now() * 0.09) * 0.006 * blocked : 0);

    // pawl follows the rim up, then falls and bounces when a tooth passes
    const rimLift = (rimAt(ALPHA - theta) - R1) / liftPerRad;
    if (rimLift >= pawlAng) {
      pawlAng = rimLift;
      pawlVel = 0;
    } else {
      pawlVel -= 70 * dt;
      pawlAng += pawlVel * dt;
      if (pawlAng < rimLift) {
        if (pawlVel < -1.2) flash = 1;
        pawlAng = rimLift;
        pawlVel = -pawlVel * 0.28;
      }
    }
    pawlPivot.rotation.z = armAngle + liftDir * pawlAng;

    const a = new THREE.Vector3().copy(springLocal).applyMatrix4(
      new THREE.Matrix4().makeRotationZ(pawlPivot.rotation.z).setPosition(pawlPivot.position),
    );
    const dir = new THREE.Vector3().subVectors(anchor, a);
    spring.position.copy(a);
    spring.scale.set(1, dir.length(), 1);
    spring.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), dir.normalize());

    flash = Math.max(0, flash - dt * 5);
    glow.intensity = 10 + flash * 22;

    walk += omega * 24 * dt;
    if (petMat.map) petMat.map.offset.x = (Math.floor(walk) % 4) * 0.25;
    hopTime = Math.max(0, hopTime - dt);
    hop = hopTime > 0 ? Math.sin(Math.PI * (1 - hopTime / HOP)) * 0.28 : 0;

    pointerSmooth.lerp(pointer, 1 - Math.exp(-dt * 3));
    camera.position.x = pointerSmooth.x * 0.25;
    camera.position.y = pointerSmooth.y * 0.18;
    camera.lookAt(0, 0, 0);
    layout();
  }

  function frame() {
    const dt = Math.min(delta(), 1 / 20);
    update(dt);
    renderer.render(scene, camera);
  }

  function loop() {
    if (!running) return;
    frame();
    requestAnimationFrame(loop);
  }

  function requestRender() {
    needsRender = true;
    if (!running) {
      requestAnimationFrame(() => {
        if (needsRender && !running) {
          needsRender = false;
          delta();
          update(0);
          renderer.render(scene, camera);
        }
      });
    }
  }

  function start() {
    if (running) return;
    running = true;
    delta();
    requestAnimationFrame(loop);
  }
  function stop() {
    running = false;
  }

  function nextPet() {
    speciesIndex = (speciesIndex + 1) % SPECIES.length;
    petMat.map = texture(SPECIES[speciesIndex]);
    petMat.needsUpdate = true;
    hopTime = HOP;
    requestRender();
    return SPECIES[speciesIndex];
  }

  canvas.addEventListener("click", nextPet);
  window.addEventListener("pointermove", (e) => {
    pointer.set((e.clientX / window.innerWidth) * 2 - 1, -((e.clientY / window.innerHeight) * 2 - 1));
  }, { passive: true });
  window.addEventListener("resize", resize);
  resize();

  return {
    state,
    start,
    stop,
    nextPet,
    requestRender,
    setScrollVelocity(v) { scrollVel = v; },
  };
}
