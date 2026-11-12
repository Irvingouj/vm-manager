# IT does not gate Applications before they go live; it sees all of them and can stop any of them

Nothing an employee does to an Application waits for IT: creating it, declaring the Resources it uses, deploying it, sharing it, adding Contributors. In exchange, IT always has the whole picture and the last word. It can list every Application with the Resources it declares, its Contributors and whether anyone still uses it, and it can disable any Application at once. We chose this because with every employee able to build, review before release cannot keep up, while visibility and the power to stop scale without effort.

Declared access is what makes the picture cheap: "which Applications touch the HR database" is a lookup, not an investigation.

The single exception is releasing an Integration function (ADR 0008), because an Integration acts beyond the access of the User it serves.

## Considered Options

- **Review before release**: rejected throughout this design (see ADR 0003); it makes IT's workload grow with the number of Applications.
- **Logs only**: IT could reconstruct what happened but has no inventory and cannot stop anything.
