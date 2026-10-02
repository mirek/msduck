import {test} from 'node:test'
import {registerGuidAssignment} from '../guid_assignment_runtime.test.mjs'

// Register in this entry point so periodic CI discovery and shard accounting
// use the same file identity as the executed test.
registerGuidAssignment((name,options,run)=>test(name,options,run))
