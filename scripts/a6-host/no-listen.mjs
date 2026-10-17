// Guard the production TS cross-check without installing module-loader hooks.
import { Server } from 'node:net';
Server.prototype.listen = function () {
  throw new Error('A6 host stdio cross-check forbids TCP listen');
};
