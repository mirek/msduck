import {withReferenceContainer} from './lib/reference-container.mjs';
import {connect,command} from './lib/reference.mjs';
import {capture,canonical} from './lib/compatibility.mjs';
import {writeFile} from 'node:fs/promises';

await withReferenceContainer(async(config,container)=>{
 const c=await connect(config);
 try {
  await command(c,'SET ARITHABORT ON; SET ANSI_WARNINGS ON');
  const version=canonical(await command(c,"SELECT CONVERT(NVARCHAR(128),SERVERPROPERTY('ProductVersion')) AS version"));
  const cases=[];
  for(const [width,min,max] of [['INT','-2147483648','2147483647'],['BIGINT','-9223372036854775808','9223372036854775807']]) {
   const pairs=[['0','0'],[null,'0'],['0',null],[null,null],[max,'1'],[min,'-1'],[min,'1'],[max,'2'],['7','-3'],['-7','3'],['-7','-3'],['1','2'],[min,min],[max,max],[null,min],[min,null]];
   for(const operation of ['+','-','*','/','%']) for(const [left,right] of pairs) {
    const operand=value=>`CAST(${value===null?'NULL':`'${value}'`} AS ${width})`;
    const sql=`SELECT ${operand(left)} ${operation} ${operand(right)} AS value`;
    cases.push({width,operation,left,right,sql,result:canonical(await capture(c,sql))});
   }
  }
  for(const [leftWidth,rightWidth,min,max,rightMax] of [
   ['INT','BIGINT','-2147483648','2147483647','9223372036854775807'],
   ['BIGINT','INT','-9223372036854775808','9223372036854775807','2147483647'],
  ]) for(const operation of ['+','-','*','/','%']) for(const [left,right] of [[min,'-1'],[max,'1'],[null,'0'],['0',null],[max,rightMax],['-7','3'],['7','-3'],['-7','-3']]) {
   const operand=(value,width)=>`CAST(${value===null?'NULL':`'${value}'`} AS ${width})`;
   const sql=`SELECT ${operand(left,leftWidth)} ${operation} ${operand(right,rightWidth)} AS value`;
   cases.push({width:'BIGINT',leftWidth,rightWidth,operation,left,right,sql,result:canonical(await capture(c,sql))});
  }
  await writeFile(process.argv[2]??'reference/checked-integer.json',JSON.stringify({image:container.image,version,settings:{arithabort:'ON',ansiWarnings:'ON'},cases},null,2)+'\n');
  console.log('Captured',cases.length,'checked integer cases');
 } finally {await new Promise(resolve=>{c.once('end',resolve);c.close();});}
});
