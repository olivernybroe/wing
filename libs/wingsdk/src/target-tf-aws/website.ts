import { readdirSync } from "fs";
import { extname, join, posix, resolve, sep } from "path";

import { Construct } from "constructs";
import mime from "mime-types";
import { createEncryptedBucket } from "./bucket";
import { core } from "..";
import { CloudfrontDistribution } from "../.gen/providers/aws/cloudfront-distribution";
import { CloudfrontOriginAccessControl } from "../.gen/providers/aws/cloudfront-origin-access-control";
import { DataAwsIamPolicyDocument } from "../.gen/providers/aws/data-aws-iam-policy-document";
import { S3Bucket } from "../.gen/providers/aws/s3-bucket";
import { S3BucketPolicy } from "../.gen/providers/aws/s3-bucket-policy";
import { S3BucketWebsiteConfiguration } from "../.gen/providers/aws/s3-bucket-website-configuration";
import { S3Object } from "../.gen/providers/aws/s3-object";
import * as cloud from "../cloud";
import { Json } from "../std";

const INDEX_FILE = "index.html";

/**
 * AWS implementation of `cloud.Website`.
 *
 * @inflight `@winglang/sdk.cloud.IWebsiteClient`
 */
export class Website extends cloud.Website {
  private readonly bucket: S3Bucket;
  private readonly _url: string;

  constructor(scope: Construct, id: string, props: cloud.WebsiteProps) {
    super(scope, id, props);

    this.bucket = createEncryptedBucket(this, false, "WebsiteBucket");

    new S3BucketWebsiteConfiguration(this, "BucketWebsiteConfiguration", {
      bucket: this.bucket.bucket,
      indexDocument: { suffix: INDEX_FILE },
    });

    this.uploadFiles(this.path);

    // create a cloudfront oac
    const cloudfrontOac = new CloudfrontOriginAccessControl(
      this,
      "CloudfrontOac",
      {
        name: "cloudfront-oac",
        originAccessControlOriginType: "s3",
        signingBehavior: "always",
        signingProtocol: "sigv4",
      }
    );

    // create a cloudFront distribution
    const distribution = new CloudfrontDistribution(this, "Distribution", {
      enabled: true,
      ...(this._domain && { aliases: [this._domain] }),
      origin: [
        {
          domainName: this.bucket.bucketRegionalDomainName,
          originId: "s3Origin",
          originAccessControlId: cloudfrontOac.id,
        },
      ],
      defaultRootObject: INDEX_FILE,
      defaultCacheBehavior: {
        allowedMethods: ["GET", "HEAD"],
        cachedMethods: ["GET", "HEAD"],
        targetOriginId: "s3Origin",
        forwardedValues: {
          queryString: false,
          cookies: { forward: "none" },
        },
        compress: true,
        viewerProtocolPolicy: "redirect-to-https",
        minTtl: 0,
        defaultTtl: 3600,
        maxTtl: 86400,
      },
      restrictions: {
        geoRestriction: {
          locations: [],
          restrictionType: "none",
        },
      },
      priceClass: "PriceClass_100",
      viewerCertificate: { cloudfrontDefaultCertificate: true },
    });

    // allow cloudfront distribution to read from private s3 bucket
    const allowDistributionReadOnly = new DataAwsIamPolicyDocument(
      this,
      "AllowDistributionReadOnly",
      {
        statement: [
          {
            actions: ["s3:GetObject"],
            condition: [
              {
                test: "StringEquals",
                values: [distribution.arn],
                variable: "AWS:SourceArn",
              },
            ],
            principals: [
              {
                identifiers: ["cloudfront.amazonaws.com"],
                type: "Service",
              },
            ],
            resources: [`${this.bucket.arn}/*`],
          },
        ],
      }
    );

    // attach policy to s3 bucket
    new S3BucketPolicy(this, "DistributionS3BucketPolicy", {
      bucket: this.bucket.id,
      policy: allowDistributionReadOnly.json,
    });

    this._url = distribution.domainName;
  }

  public get url(): string {
    return this._url;
  }

  public addJson(path: string, data: Json): string {
    if (!path.endsWith(".json")) {
      throw new Error(
        `key must have a .json suffix. (current: "${path.split(".").pop()}")`
      );
    }

    new S3Object(this, `File-${path}`, {
      dependsOn: [this.bucket],
      content: JSON.stringify(data),
      bucket: this.bucket.bucket,
      contentType: "application/json",
      key: this.formatPath(path),
    });

    return `${this.url}/${path}`;
  }

  private uploadFile(filePath: string) {
    const fileKey = filePath.replace(this.path, "");

    new S3Object(this, `File${fileKey.replace(/[\/\\]/g, "--")}`, {
      dependsOn: [this.bucket],
      key: this.formatPath(filePath.replace(this.path, "")),
      bucket: this.bucket.bucket,
      source: resolve(filePath),
      contentType: mime.contentType(extname(filePath)) || undefined,
    });
  }

  private uploadFiles(dir: string): void {
    const files = readdirSync(dir, { withFileTypes: true });
    for (const file of files) {
      const filename = join(dir, file.name);
      if (file.isDirectory()) {
        this.uploadFiles(filename);
      } else {
        this.uploadFile(filename);
      }
    }
  }

  private formatPath(path: string): string {
    return path.split(sep).join(posix.sep);
  }

  /** @internal */
  public _toInflight(): core.Code {
    return core.InflightClient.for(
      __dirname.replace("target-tf-aws", "shared-aws"),
      __filename,
      "WebsiteClient",
      []
    );
  }
}
