# Math Demo

A gallery of math rendered through `remark-math` + `rehype-katex`, intended
both as a visual check for the KaTeX theme wiring and as a smoke test for
the inline / block math paths in the rendered-Markdown editor. Keeping
expression count under the per-document budget documented in
`docs/features/source-renderers.md`.

## Inline math

Simple inline: Euler's identity is $e^{i\pi} + 1 = 0$.

Interleaved with prose: the golden ratio $\varphi = \tfrac{1 + \sqrt{5}}{2}$
satisfies $\varphi^2 = \varphi + 1$, which is often used to approximate
asymptotic growth like $F_n \sim \varphi^n / \sqrt{5}$ for the Fibonacci
numbers.

Physics constants in running text: the fine-structure constant is
$\alpha \approx 1/137.036$; the Planck relation $E = h\nu$ ties a photon's
energy to its frequency; the speed of light $c = 299\,792\,458$ m/s is
exact by definition.

## Block math

A canonical sum:

$$
\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}
$$

A limit definition of the derivative:

$$
f'(x) = \lim_{h \to 0} \frac{f(x + h) - f(x)}{h}
$$

The fundamental theorem of calculus:

$$
\int_{a}^{b} f'(x) \, dx = f(b) - f(a)
$$

## Fractions and roots

Nested fractions:

$$
\cfrac{1}{1 + \cfrac{1}{2 + \cfrac{1}{3 + \cfrac{1}{4 + \cdots}}}}
$$

Heron-style roots:

$$
\sqrt[n]{a} = a^{1/n}, \qquad \sqrt{\sqrt{x}} = x^{1/4}
$$

## Greek and operators

$$
\alpha, \beta, \gamma, \delta, \epsilon, \zeta, \eta, \theta, \lambda,
\mu, \nu, \xi, \pi, \rho, \sigma, \tau, \phi, \chi, \psi, \omega
$$

$$
\nabla \cdot \vec{E} = \frac{\rho}{\varepsilon_0}, \qquad
\nabla \times \vec{B} - \frac{1}{c^2}\frac{\partial \vec{E}}{\partial t}
= \mu_0 \vec{J}
$$

## Matrices

A $3 \times 3$ rotation matrix about the $z$ axis:

$$
R_z(\theta) =
\begin{pmatrix}
\cos\theta & -\sin\theta & 0 \\
\sin\theta &  \cos\theta & 0 \\
0 & 0 & 1
\end{pmatrix}
$$

A block-diagonal system:

$$
\begin{bmatrix}
A & 0 \\
0 & B
\end{bmatrix}
\begin{bmatrix} x \\ y \end{bmatrix}
=
\begin{bmatrix} Ax \\ By \end{bmatrix}
$$

A determinant:

$$
\det
\begin{vmatrix}
a & b \\
c & d
\end{vmatrix}
= ad - bc
$$

## Cases and piecewise

The Heaviside step function:

$$
H(x) =
\begin{cases}
0 & \text{if } x < 0 \\
\tfrac{1}{2} & \text{if } x = 0 \\
1 & \text{if } x > 0
\end{cases}
$$

## Probability and statistics

The normal probability density:

$$
f(x \mid \mu, \sigma^2) = \frac{1}{\sqrt{2\pi\sigma^2}}\,
\exp\!\left(-\frac{(x - \mu)^2}{2\sigma^2}\right)
$$

Variance decomposition (law of total variance):

$$
\mathrm{Var}(Y) = \mathrm{E}\!\big[\mathrm{Var}(Y \mid X)\big]
+ \mathrm{Var}\!\big(\mathrm{E}[Y \mid X]\big)
$$

Entropy of a discrete distribution:

$$
H(X) = -\sum_{i} p_i \log_2 p_i
$$

## Machine learning

Softmax over a logit vector $\vec{z} \in \mathbb{R}^K$:

$$
\mathrm{softmax}(\vec{z})_i = \frac{e^{z_i}}{\sum_{j=1}^{K} e^{z_j}}
$$

Cross-entropy loss for a one-hot target $y$:

$$
\mathcal{L}(\theta) = -\sum_{i=1}^{K} y_i \log \hat{y}_i(\theta)
$$

Gradient-descent update rule with learning rate $\eta$:

$$
\theta_{t+1} = \theta_t - \eta \, \nabla_{\theta} \mathcal{L}(\theta_t)
$$

## Aligned multi-line equations

$$
\begin{aligned}
(a + b)^2 &= a^2 + 2ab + b^2 \\
(a - b)^2 &= a^2 - 2ab + b^2 \\
(a + b)(a - b) &= a^2 - b^2
\end{aligned}
$$

A derivation with annotations:

$$
\begin{aligned}
\int_0^{\infty} e^{-x^2} \, dx
  &= \tfrac{1}{2} \int_{-\infty}^{\infty} e^{-x^2} \, dx
     && \text{(even integrand)} \\
  &= \tfrac{1}{2} \sqrt{\pi}
     && \text{(Gaussian integral)}
\end{aligned}
$$

## Cross-check with prose

Math inside a sentence should flow naturally: if
$\mathbf{A} \in \mathbb{R}^{m \times n}$ has singular value decomposition
$\mathbf{A} = U \Sigma V^\top$, then the best rank-$k$ approximation in
Frobenius norm is obtained by truncating $\Sigma$ to its top $k$ singular
values — the classical Eckart–Young theorem.

Mixing with code: the quadratic formula
$x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}$ corresponds to the
implementation:

```python
import math

def roots(a: float, b: float, c: float) -> tuple[float, float]:
    disc = b * b - 4 * a * c
    s = math.sqrt(disc)
    return ((-b + s) / (2 * a), (-b - s) / (2 * a))
```

## Mixed inline and block

The Riemann zeta function
$\zeta(s) = \sum_{n=1}^{\infty} n^{-s}$ converges for
$\mathrm{Re}(s) > 1$. Its famous functional equation is:

$$
\zeta(s) = 2^s \pi^{s-1} \sin\!\left(\tfrac{\pi s}{2}\right) \Gamma(1-s)\,\zeta(1-s)
$$

which reflects values across the critical line $\mathrm{Re}(s) = \tfrac{1}{2}$.
