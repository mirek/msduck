// Capture exact behavior from a fresh pinned SQL Server container.
import {withReferenceContainer} from './lib/reference-container.mjs';
import {connect} from './lib/reference.mjs';
import {capture,canonical} from './lib/compatibility.mjs';
import {writeFile} from 'node:fs/promises';
await withReferenceContainer(async(config,container)=>{
 const c=await connect(config); const cases=[];
 try {
  for(const source of ['0x','0x41','0x4100','0x410042','0x41004200','0x00D8','0x00DC','0x3DD800DE','CAST(NULL AS VARBINARY(MAX))']) {
   for(const type of ['NVARCHAR(MAX)','NVARCHAR(1)','NCHAR(3)','NVARCHAR','NCHAR']) {
    const expr=`CONVERT(${type},${source})`;
    const sql=`SELECT ${expr} AS value,CONVERT(VARBINARY(MAX),${expr}) AS raw,DATALENGTH(${expr}) AS bytes`;
    cases.push({source,type,sql,result:canonical(await capture(c,sql))});
   }
  }
  for(const style of ['0','1','2','3','NULL','-1']) for(const trying of [false,true]) {
    const expr=`${trying?'TRY_CONVERT':'CONVERT'}(NVARCHAR(5),0x410042,${style})`;
    const sql=`SELECT ${expr} AS value,CONVERT(VARBINARY(MAX),${expr}) AS raw`;
    cases.push({style,trying,sql,result:canonical(await capture(c,sql))});
  }
  for(const source of ['0x','0xAB','0xABCD01']) for(const style of [1,2]) {
   for(const type of ['NVARCHAR(1)','NVARCHAR(2)','NVARCHAR(3)','NVARCHAR(4)','NVARCHAR(5)','NVARCHAR(6)','NVARCHAR(7)','NVARCHAR(8)','NCHAR(1)','NCHAR(3)','NCHAR(5)','NCHAR(8)','NVARCHAR(MAX)']) {
    const expr=`CONVERT(${type},${source},${style})`;
    const sql=`SELECT ${expr} AS value,CONVERT(VARBINARY(MAX),${expr}) AS raw,DATALENGTH(${expr}) AS bytes`;
    cases.push({source,type,style,sql,result:canonical(await capture(c,sql))});
   }
  }
  await writeFile(process.argv[2] ?? 'tests/reference/binary-unicode.json',JSON.stringify({image:container.image,cases},null,2)+'\n');
  console.log('Captured',cases.length,'binary-to-Unicode cases');
 } finally {c.close();}
});
