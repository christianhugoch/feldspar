// Whether this server has the builder bundle (TODO "The builder" §2, §9).

import { useEffect, useState } from "react";

import { api } from "./api";

/** Whether the builder routes have a bundle to serve, or `null` while asking.
 * A server that cannot say is taken not to have one: a link to a builder that
 * answers "not built" is the worse of the two mistakes, since the JSON is still
 * shown either way. */
export function useBuilderAvailable(): boolean | null {
  const [available, setAvailable] = useState<boolean | null>(null);
  useEffect(() => {
    api
      .builderStatus()
      .then((status) => setAvailable(status.available))
      .catch(() => setAvailable(false));
  }, []);
  return available;
}
