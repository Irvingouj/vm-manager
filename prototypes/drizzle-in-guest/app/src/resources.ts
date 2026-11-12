// In the platform this module would be generated from the Application's
// Declared access: one typed database per Resource.
import * as own from "./own/schema";
import * as devopsTables from "./devops/schema";
import * as devopsRelations from "./devops/relations";
import { platform } from "./vm";

export const vm = platform({
  app: { schema: own, access: "write" },
  devops: { schema: { ...devopsTables, ...devopsRelations }, access: "read" },
});
