// simulator.rs
//
// The raindrop simulation of raindrop-fx (https://github.com/SardineFish/raindrop-fx,
// MIT, see data/rain/LICENSE.raindrop-fx): simulator.ts, spawner.ts,
// raindrop.ts and random.ts ported line by line. The arithmetic is kept in
// the same order and in f64, like the JavaScript numbers, and the random
// numbers are drawn in the same order, so with the same sequence of random
// numbers and time steps the drops come out the same.
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::f64::consts::PI;

use super::Options;

/// The source of `Math.random()`: mulberry32, seeded once.
pub struct Random {
    state: u32,
    pub calls: u64,
}

impl Random {
    pub fn new(seed: u32) -> Self {
        Self {
            state: seed,
            calls: 0,
        }
    }

    /// A number in [0, 1), like `Math.random()`.
    pub fn next(&mut self) -> f64 {
        self.calls += 1;
        self.state = self.state.wrapping_add(0x6D2B_79F5);
        let a = self.state;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }

    /// `randomRange(min, max)`
    pub fn range(&mut self, [min, max]: [f64; 2]) -> f64 {
        self.next() * (max - min) + min
    }

    /// `random()`, a number in (-1, 1)
    fn signed(&mut self) -> f64 {
        self.next() * 2.0 - 1.0
    }
}

/// `Math.max`, which unlike `f64::max` gives NaN when either side is NaN.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if b > a {
        b
    } else {
        a
    }
}

/// `lerp` from utils.ts
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The time of one update, `Time` in utils.ts.
#[derive(Clone, Copy, Debug)]
pub struct Time {
    pub dt: f64,
    pub total: f64,
}

/// `RainDrop` from raindrop.ts. Drops refer to each other (the parent of a
/// trail drop) by id, and to their collision grid cell by index.
pub struct RainDrop {
    id: u64,
    pub pos: [f64; 2],
    density: f64,
    velocity: [f64; 2],
    spread: [f64; 2],
    pub destroyed: bool,
    parent: Option<u64>,
    grid: Option<usize>,
    grid_idx: usize,

    mass: f64,
    pub size: [f64; 2],
    resistance: f64,
    shifting: f64,
    last_trail_pos: [f64; 2],
    next_trail_distance: f64,
    next_random_time: f64,
}

impl RainDrop {
    fn new(
        id: u64,
        options: &Options,
        random: &mut Random,
        pos: [f64; 2],
        size: f64,
        density: f64,
    ) -> Self {
        let next_trail_distance = random.range(options.trail_distance);
        let mut drop = Self {
            id,
            pos,
            density,
            velocity: [0.0, 0.0],
            spread: [options.initial_spread, options.initial_spread],
            destroyed: false,
            parent: None,
            grid: None,
            grid_idx: 0,
            mass: 0.0,
            size: [0.0, 0.0],
            resistance: 0.0,
            shifting: 0.0,
            last_trail_pos: pos,
            next_trail_distance,
            next_random_time: 0.0,
        };
        let m = size * density;
        drop.set_mass(m * m);
        drop
    }

    /// The `mass` setter, which also sets the size.
    fn set_mass(&mut self, m: f64) {
        self.mass = m;
        let sqrt_m = m.sqrt() / self.density;
        self.size[0] = (self.spread[0] + 1.0) * sqrt_m;
        self.size[1] = (self.spread[1] + 1.0) * sqrt_m;
    }

    fn merge_distance(&self, options: &Options) -> f64 {
        self.size[0] * (1.0 + self.spread[0]) * 0.16 * options.collider_size
    }

    fn random_motion(&mut self, options: &Options, random: &mut Random) {
        let max_resistance = lerp(
            options.spawn_size[0],
            options.spawn_size[1],
            1.0 - options.slip_rate,
        );
        let max_resistance = max_resistance * max_resistance * 4.0;
        self.resistance = random.range([0.0, 1.0]) * options.gravity * max_resistance;
        let direction = random.signed();
        self.shifting = direction * random.range(options.x_shifting);
    }

    /// `updateRaindrop()` up to the trail check; true when the drop should
    /// split off a trail drop.
    fn update(&mut self, options: &Options, random: &mut Random, time: Time) -> bool {
        if self.next_random_time <= time.total {
            self.next_random_time = time.total + random.range(options.motion_interval);
            self.random_motion(options, random);
        }

        self.set_mass(self.mass - options.evaporate * time.dt);
        let force = options.gravity * self.mass - self.resistance;
        let acceleration = force / self.mass;
        self.velocity[1] -= acceleration * time.dt;
        if self.velocity[1] > 0.0 {
            self.velocity[1] = 0.0;
        }
        self.velocity[0] = self.velocity[1].abs() * self.shifting;
        self.pos[0] += self.velocity[0] * time.dt;
        self.pos[1] += self.velocity[1] * time.dt;

        let spread_by_velocity =
            options.velocity_spread * 2.0 * (self.velocity[1] * 0.005).abs().atan() / PI;
        self.spread[1] = js_max(self.spread[1], spread_by_velocity);
        self.spread[0] *= options.shrink_rate.powf(time.dt);
        self.spread[1] *= options.shrink_rate.powf(time.dt);

        let dx = self.last_trail_pos[0] - self.pos[0];
        let dy = self.last_trail_pos[1] - self.pos[1];
        dx * dx + dy * dy > self.next_trail_distance * self.next_trail_distance
    }

    fn merge(&mut self, target_velocity: [f64; 2], target_mass: f64) {
        let self_momentum = [self.velocity[0] * self.mass, self.velocity[1] * self.mass];
        let target_momentum = [
            target_velocity[0] * target_mass,
            target_velocity[1] * target_mass,
        ];
        let momentum = [
            self_momentum[0] + target_momentum[0],
            self_momentum[1] + target_momentum[1],
        ];
        self.set_mass(self.mass + target_mass);
        self.velocity = [momentum[0] / self.mass, momentum[1] / self.mass];
    }
}

/// `RaindropSimulator` with its `Spawner`. The drops live in a slab; `list`
/// is the `raindrops` array and `grid` the collision grid, both holding slab
/// indices in the same order as the arrays upstream.
pub struct Simulator {
    options: Options,
    viewport: [f64; 2],
    drops: Vec<RainDrop>,
    free: Vec<usize>,
    list: Vec<usize>,
    grid: Vec<Vec<usize>>,
    next_id: u64,
    current_time: f64,
    next_spawn: f64,
}

impl Simulator {
    pub fn new(options: Options, width: f64, height: f64) -> Self {
        let mut simulator = Self {
            options,
            viewport: [width, height],
            drops: Vec::new(),
            free: Vec::new(),
            list: Vec::new(),
            grid: Vec::new(),
            next_id: 0,
            current_time: 0.0,
            next_spawn: 0.0,
        };
        simulator.resize_grid();
        simulator
    }

    /// The viewport follows the view. Upstream only sizes the collision grid
    /// in the constructor, so after the view has grown the drops above the
    /// first size never merge; here the grid grows with the view.
    pub fn resize(&mut self, width: f64, height: f64) {
        self.viewport = [width, height];
        self.resize_grid();
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// The drops to draw, in order: position and size.
    pub fn raindrops(&self) -> impl ExactSizeIterator<Item = &RainDrop> {
        self.list.iter().map(|&i| &self.drops[i])
    }

    fn grid_size(&self) -> f64 {
        self.options.spawn_size[1] * 0.3
    }

    fn resize_grid(&mut self) {
        let w = (self.viewport[0] / self.grid_size()).ceil();
        let h = (self.viewport[1] / self.grid_size()).ceil();
        let cells = (w * h).max(0.0) as usize;
        if self.grid.len() < cells {
            self.grid.resize_with(cells, Vec::new);
        }
    }

    fn grid_at(&self, grid_x: f64, grid_y: f64) -> Option<usize> {
        if grid_x < 0.0 || grid_y < 0.0 {
            return None;
        }
        let grid_width = (self.viewport[0] / self.grid_size()).ceil();
        let idx = grid_y * grid_width + grid_x;
        if idx.is_nan() || idx >= self.grid.len() as f64 {
            return None;
        }
        Some(idx as usize)
    }

    fn world_to_grid(&self, [x, y]: [f64; 2]) -> (f64, f64) {
        (
            (x / self.grid_size()).floor(),
            (y / self.grid_size()).floor(),
        )
    }

    fn grid_add(&mut self, cell: usize, drop: usize) {
        self.grid[cell].push(drop);
        self.drops[drop].grid_idx = self.grid[cell].len() - 1;
        self.drops[drop].grid = Some(cell);
    }

    fn grid_delete(&mut self, drop: usize) {
        let Some(cell) = self.drops[drop].grid else {
            return;
        };
        let idx = self.drops[drop].grid_idx;
        let grid = &mut self.grid[cell];
        let last = grid[grid.len() - 1];
        grid[idx] = last;
        grid.pop();
        self.drops[last].grid_idx = idx;
        self.drops[drop].grid = None;
    }

    fn alloc(&mut self, drop: RainDrop) -> usize {
        if let Some(i) = self.free.pop() {
            self.drops[i] = drop;
            i
        } else {
            self.drops.push(drop);
            self.drops.len() - 1
        }
    }

    fn new_drop(&mut self, random: &mut Random, pos: [f64; 2], size: f64, density: f64) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let drop = RainDrop::new(id, &self.options, random, pos, size, density);
        self.alloc(drop)
    }

    /// `add()`: a trail drop goes straight into the collision grid.
    fn add(&mut self, drop: usize) {
        self.list.push(drop);
        let (x, y) = self.world_to_grid(self.drops[drop].pos);
        if let Some(cell) = self.grid_at(x, y) {
            self.grid_add(cell, drop);
        }
    }

    pub fn update(&mut self, random: &mut Random, time: Time) {
        if self.list.len() as f64 <= self.options.spawn_limit {
            // Spawner.update() and trySpawn()
            self.current_time += time.dt;
            while self.current_time >= self.next_spawn {
                let size = random.range(self.options.spawn_size);
                let x = random.next() * self.viewport[0] + 0.0;
                let y = random.next() * self.viewport[1] + 0.0;
                self.next_spawn += random.range(self.options.spawn_interval);
                let drop = self.new_drop(random, [x, y], size, 1.0);
                self.list.push(drop);
            }
        }

        self.raindrop_update(random, time);
        self.collision_update();

        // The drop moved into a freed place is not looked at again until
        // the next update, as upstream.
        let mut i = 0;
        while i < self.list.len() {
            let drop = self.list[i];
            if self.drops[drop].destroyed {
                self.grid_delete(drop);
                let last = self.list[self.list.len() - 1];
                self.list[i] = last;
                self.list.pop();
                self.free.push(drop);
            }
            i += 1;
        }
    }

    fn raindrop_update(&mut self, random: &mut Random, time: Time) {
        // Trail drops added on the way are updated in the same pass.
        let mut i = 0;
        while i < self.list.len() {
            let drop = self.list[i];
            i += 1;
            if self.drops[drop].destroyed {
                continue;
            }

            if self.drops[drop].update(&self.options, random, time) {
                self.split(random, drop);
            }
            if self.drops[drop].pos[1] < -100.0 {
                self.drops[drop].destroyed = true;
                continue;
            }

            let (x, y) = self.world_to_grid(self.drops[drop].pos);
            let cell = self.grid_at(x, y);
            if cell != self.drops[drop].grid {
                self.grid_delete(drop);
                if let Some(cell) = cell {
                    self.grid_add(cell, drop);
                }
            }
        }
    }

    /// `split()`: leave a small trail drop behind.
    fn split(&mut self, random: &mut Random, drop: usize) {
        let options = self.options;
        let (size, pos, velocity_y) = {
            let d = &self.drops[drop];
            if d.mass < 1000.0 {
                return;
            }
            let size = d.size[0] * random.range(options.trail_drop_size);
            let pos = [
                random.range([-5.0, 5.0]) + d.pos[0],
                d.size[1] / 4.0 + d.pos[1],
            ];
            (size, pos, d.velocity[1])
        };
        let trail = self.new_drop(random, pos, size, options.trail_drop_density);
        let parent_id = self.drops[drop].id;
        let trail_mass = {
            let t = &mut self.drops[trail];
            t.spread = [0.1, velocity_y.abs() * 0.01 * options.trail_spread];
            t.parent = Some(parent_id);
            t.mass
        };
        {
            let d = &mut self.drops[drop];
            d.set_mass(d.mass - trail_mass);
        }
        self.add(trail);
        let d = &mut self.drops[drop];
        d.last_trail_pos = d.pos;
        d.next_trail_distance = random.range(options.trail_distance);
    }

    fn collision_update(&mut self) {
        let options = self.options;
        for i in 0..self.list.len() {
            let raindrop = self.list[i];
            if self.drops[raindrop].destroyed {
                continue;
            }
            let (grid_x, grid_y) = self.world_to_grid(self.drops[raindrop].pos);

            for x in -1..=1 {
                for y in -1..=1 {
                    let Some(cell) = self.grid_at(grid_x + f64::from(x), grid_y + f64::from(y))
                    else {
                        continue;
                    };
                    let mut k = 0;
                    while k < self.grid[cell].len() {
                        let other = self.grid[cell][k];
                        k += 1;

                        let r = &self.drops[raindrop];
                        let o = &self.drops[other];
                        let is_same = other == raindrop;
                        let is_parent = o.parent == Some(r.id) || r.parent == Some(o.id);
                        let is_adjacent = r.parent.is_some() && r.parent == o.parent;
                        if o.destroyed || is_parent || is_adjacent || is_same {
                            continue;
                        }

                        let dx = r.pos[0] - o.pos[0];
                        let dy = r.pos[1] - o.pos[1];
                        let distance = (dx * dx + dy * dy).sqrt();
                        if distance - r.merge_distance(&options) - o.merge_distance(&options) < 0.0
                        {
                            // The drop that loses is only marked here and
                            // can still take in others later in this pass.
                            if r.mass >= o.mass {
                                let (velocity, mass) = (o.velocity, o.mass);
                                self.drops[raindrop].merge(velocity, mass);
                                self.drops[other].destroyed = true;
                            } else {
                                let (velocity, mass) = (r.velocity, r.mass);
                                self.drops[other].merge(velocity, mass);
                                self.drops[raindrop].destroyed = true;
                            }
                        }
                    }
                }
            }
        }
    }
}
