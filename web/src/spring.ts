/**
 * Critically damped spring (Apple's damping ratio 1.0): no overshoot, and retargeting keeps the
 * current velocity, so an animation can be redirected mid-flight without a jolt. `response` is in
 * seconds; lower is snappier.
 */
export class Spring {
  velocity = 0;
  target: number;

  constructor(
    public value: number,
    private readonly response = 0.35,
  ) {
    this.target = value;
  }

  /** Advances by `dt` seconds. */
  step(dt: number): void {
    const omega = (2 * Math.PI) / this.response;
    // Semi-implicit Euler in small substeps stays stable at any frame rate.
    for (let left = dt; left > 0; left -= SUBSTEP) {
      const h = Math.min(SUBSTEP, left);
      this.velocity += (-omega * omega * (this.value - this.target) - 2 * omega * this.velocity) * h;
      this.value += this.velocity * h;
    }
  }

  /** True once within `epsilon` of the target and nearly still; snaps onto the target. */
  settled(epsilon: number): boolean {
    if (Math.abs(this.value - this.target) > epsilon || Math.abs(this.velocity) > epsilon * 10) return false;
    this.value = this.target;
    this.velocity = 0;
    return true;
  }
}

const SUBSTEP = 1 / 240;
