# ISSLIVE: attitude convention and unit corrections (the design document R4)

Checked 2026-10-23 against the live feed (`mctl isslive`, 20 s capture), not against documentation.

**Quaternion convention.** Reading USLAB000028-31 (the *commanded* attitude) as scalar-first, aerospace
3-2-1 (roll, pitch, yaw) gives roll 0.9000, pitch -7.8000, yaw -4.0000 degrees to better than 1e-6 of a
degree. Commanded attitudes are chosen as round numbers, so a wrong component order or sequence would
not produce this. The current attitude (USLAB000018-21) read the same way gave 0.39, -7.72, -3.94.

**Units.** The spreadsheet lists the error items (USLAB000022-24) and rates (USLAB000025-27) as RAD and
RAD/S while their descriptions say deg and deg/s. The values say degrees:
- Current minus commanded attitude (above) is roll -0.510, pitch +0.078, yaw +0.058 deg; the roll error
  item read -0.499 deg in the same capture. In radians that would be 29 degrees.
- Inertial rate Y read -0.0645 and X, Z ~0. The orbital rate is 360 deg / ~92.6 min = 0.065 deg/s, and
  a LVLH-holding vehicle rotates once per orbit about the pitch axis. In rad/s it would be 3.7 deg/s.
`config/isslive.toml` therefore carries deg and deg/s for those six items.

**What is checked at run time.** The GNC page recomputes current-minus-commanded from the quaternions
and compares it with the reported error items (tolerance `ADI_CONSISTENCY_TOL_DEG`). A disagreement is
shown as a WARNING on the page.

**Still not proven.** That the displayed sense matches a crew display (nose-up positive pitch, right-wing-
down positive roll). That is a drawing convention checked by reading the code, not against the vehicle.
Pitch and yaw errors differed from the quaternion difference by up to 0.05 deg, consistent with the
items being sampled a few seconds apart.

## Atmosphere units

The spreadsheet lists Lab ppO2/ppN2/ppCO2, cabin pressure, Node 3 partial pressures and airlock pressure
as PSIA/PSI. The live values are 165.8, 577.6, 2.4, 749.1 (lab) and 150.7, 593.9, 2.9 (node 3), 749.4
(airlock). A cabin at 749 psi is impossible; 749 mmHg is 14.5 psi, ppO2 165.8 mmHg is 21.7 % of 760 and
ppN2 577.6 is 76 %. So these items are mmHg. Cabin temperature (labelled CNT) reads 23.8, i.e. degC.
Not corrected, because no evidence either way: NODE3000011 (O2 production rate, labelled LBM/D) read
-59.5, a negative production rate, which is not physical; the table shows it as published.
